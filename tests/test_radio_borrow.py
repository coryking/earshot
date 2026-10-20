"""offload/radio-borrow: the live service steps aside for a command and comes back afterwards.

The real systemctl is replaced by a fake that keeps the unit's state in a file and logs every
call, so these run anywhere with bash and flock.
"""

import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "offload"))
import radio_lock

BORROW = Path(__file__).resolve().parent.parent / "offload" / "radio-borrow"

FAKE_SYSTEMCTL = r"""#!/usr/bin/env bash
# fake: systemctl --user show -p LoadState|ActiveState --value UNIT | stop UNIT | start UNIT
state="$FAKE_DIR/state"; log="$FAKE_DIR/calls"
shift  # --user
case "$1" in
  show)
    [ -e "$FAKE_DIR/no-bus" ] && { echo "Failed to connect to bus" >&2; exit 1; }
    case "$3" in
      LoadState) [ -e "$FAKE_DIR/not-installed" ] && echo not-found || echo loaded ;;
      ActiveState) cat "$state" ;;
    esac ;;
  stop)
    echo "stop" >> "$log"
    [ -e "$FAKE_DIR/stop-fails" ] && exit 1
    echo inactive > "$state" ;;
  start) echo "start" >> "$log"; echo active > "$state" ;;
esac
"""


@pytest.fixture
def world(tmp_path):
    fake = tmp_path / "fake"
    run = tmp_path / "run"
    fake.mkdir()
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    systemctl = bin_dir / "systemctl"
    systemctl.write_text(FAKE_SYSTEMCTL)
    systemctl.chmod(0o755)
    (fake / "state").write_text("active\n")
    env = {
        **os.environ,
        "PATH": f"{bin_dir}:{os.environ['PATH']}",
        "FAKE_DIR": str(fake),
        "XDG_RUNTIME_DIR": str(run),
        "PLUTO_RADIO_LOCK": str(tmp_path / "radio.lock"),
        "PLUTO_RADIO_BORROW_LOCK": str(tmp_path / "borrow.lock"),
    }
    env.pop("PLUTO_RADIO_BORROWED", None)
    env.pop("PLUTO_RADIO_LOCK_HELD", None)
    return type("World", (), {"fake": fake, "run": run, "env": env, "tmp": tmp_path})


def borrow(world, *command, **kwargs):
    return subprocess.run(
        [str(BORROW), *command],
        check=False,
        env=world.env,
        capture_output=True,
        text=True,
        timeout=60,
        **kwargs,
    )


def calls(world) -> list[str]:
    path = world.fake / "calls"
    return path.read_text().split() if path.exists() else []


def test_live_is_stopped_for_the_command_and_started_again_afterwards(world):
    out = world.tmp / "seen"
    result = borrow(
        world,
        "bash",
        "-c",
        f'cat "$FAKE_DIR/state" > {out}; echo "$PLUTO_RADIO_BORROWED" >> {out}; exit 7',
    )
    assert result.returncode == 7, "the command's exit status is the borrower's"
    assert out.read_text().split() == ["inactive", "1"], "live was stopped while the command ran"
    assert calls(world) == ["stop", "start"]
    assert (world.fake / "state").read_text().strip() == "active"
    status = json.loads((world.run / "amateur-radio" / "receiver.json").read_text())
    assert status["state"] == "borrowed"


def test_live_that_was_not_running_is_left_stopped(world):
    (world.fake / "state").write_text("inactive\n")
    result = borrow(world, "true")
    assert result.returncode == 0
    assert calls(world) == [], "an operator who stopped live on purpose does not get it restarted"


def test_a_failed_stop_means_the_command_does_not_run(world):
    (world.fake / "stop-fails").touch()
    marker = world.tmp / "ran"
    result = borrow(world, "touch", str(marker))
    assert result.returncode == 125
    assert not marker.exists()
    assert calls(world) == ["stop"], "no restart attempt on a unit that never stopped"


def test_without_the_live_unit_the_command_just_runs(world):
    (world.fake / "not-installed").touch()
    result = borrow(world, "bash", "-c", "echo ran-$PLUTO_RADIO_BORROWED; exit 3")
    assert (result.returncode, result.stdout.strip()) == (3, "ran-1")
    assert calls(world) == []


def test_the_last_of_several_borrowers_resumes_live(world):
    slow = subprocess.Popen(
        [str(BORROW), "sleep", "2.5"], env=world.env, stderr=subprocess.PIPE, text=True
    )
    deadline = time.monotonic() + 20
    while "stop" not in calls(world):
        assert time.monotonic() < deadline
        time.sleep(0.05)
    # A second borrower comes and goes while the first is still running.
    quick = borrow(world, "true")
    assert quick.returncode == 0
    assert "another borrower is still running" in quick.stderr
    assert calls(world) == ["stop"], "live must stay down while a borrower is still running"
    assert slow.wait(timeout=30) == 0
    assert calls(world) == ["stop", "start"]


def test_sigterm_reaches_the_command_and_live_still_comes_back(world):
    proc = subprocess.Popen(
        [str(BORROW), "sleep", "60"], env=world.env, stderr=subprocess.PIPE, text=True
    )
    deadline = time.monotonic() + 20
    while "stop" not in calls(world):
        assert time.monotonic() < deadline
        time.sleep(0.05)
    time.sleep(0.3)
    proc.send_signal(signal.SIGTERM)
    proc.wait(timeout=30)
    assert calls(world) == ["stop", "start"]


def test_lock_mode_holds_the_radio_lock_around_the_command_only(world):
    probe = f'flock -n "{world.env["PLUTO_RADIO_LOCK"]}" true && echo free || echo held'
    result = borrow(world, "--lock", "bash", "-c", f'{probe}; echo "$PLUTO_RADIO_LOCK_HELD"')
    assert result.stdout.split() == ["held", "1"]
    after = subprocess.run(
        ["bash", "-c", probe], check=False, env=world.env, capture_output=True, text=True
    )
    assert after.stdout.strip() == "free", "the lock is released when the command ends"


def test_python_scripts_take_the_lock_themselves_unless_borrow_already_holds_it(world, monkeypatch):
    monkeypatch.setattr(radio_lock, "LOCK_PATH", world.env["PLUTO_RADIO_LOCK"])
    with radio_lock.radio_lock(timeout_s=1):
        monkeypatch.setenv("PLUTO_RADIO_LOCK_HELD", "1")
        with radio_lock.radio_lock(timeout_s=0.2):  # would time out if it tried to lock again
            pass


def test_orphaned_borrow_is_resumed_by_the_guard(world):
    # A borrower died with SIGKILL: live is down, the marker says it should be up, nobody borrows.
    (world.fake / "state").write_text("inactive\n")
    (world.run / "amateur-radio").mkdir(parents=True)
    (world.run / "amateur-radio" / "borrow-resume").touch()
    assert borrow(world, "--resume-orphaned").returncode == 0
    assert calls(world) == ["start"]
    # With a live borrower around it does nothing.
    (world.fake / "state").write_text("inactive\n")
    (world.run / "amateur-radio" / "borrow-resume").touch()
    holder = subprocess.Popen([str(BORROW), "sleep", "3"], env=world.env, stderr=subprocess.DEVNULL)
    time.sleep(1.0)
    before = calls(world)
    borrow(world, "--resume-orphaned")
    assert calls(world) == before
    holder.wait(timeout=30)


def test_ensure_borrowed_reruns_the_script_under_radio_borrow_once(monkeypatch):
    executed = []
    monkeypatch.setattr(os, "execv", lambda path, argv: executed.append((path, argv)))
    monkeypatch.delenv("PLUTO_RADIO_BORROWED", raising=False)
    radio_lock.ensure_borrowed(["python", "script.py", "--x"])
    assert executed == [(str(BORROW), [str(BORROW), "python", "script.py", "--x"])]
    executed.clear()
    monkeypatch.setenv("PLUTO_RADIO_BORROWED", "1")
    radio_lock.ensure_borrowed(["python", "script.py"])
    assert executed == [], "already under radio-borrow: no second wrapper"


def wait_for_file(path: Path, timeout: float = 20) -> str:
    deadline = time.monotonic() + timeout
    while not path.exists() or not path.read_text().strip():
        assert time.monotonic() < deadline, f"{path} never appeared"
        time.sleep(0.05)
    return path.read_text()


CHILD = """
import os, signal, sys, time
marker = sys.argv[1]
open(marker + ".pid", "w").write(str(os.getpid()))
try:
    open(marker + ".ready", "w").write("up")
    time.sleep(60)
except KeyboardInterrupt:
    open(marker, "w").write("keyboard-interrupt")
    sys.exit(3)
"""


def test_ctrl_c_reaches_the_command_so_its_own_cleanup_runs(world):
    marker = world.tmp / "marker"
    proc = subprocess.Popen(
        [str(BORROW), sys.executable, "-c", CHILD, str(marker)],
        env=world.env,
        stderr=subprocess.PIPE,
        text=True,
    )
    wait_for_file(Path(f"{marker}.ready"))
    proc.send_signal(signal.SIGINT)
    proc.wait(timeout=30)
    assert marker.read_text() == "keyboard-interrupt", (
        "Python must see KeyboardInterrupt, not an ignored SIGINT"
    )
    assert calls(world) == ["stop", "start"]


def test_lock_mode_signals_reach_the_command_itself_not_just_flock(world):
    marker = world.tmp / "marker"
    proc = subprocess.Popen(
        [str(BORROW), "--lock", sys.executable, "-c", CHILD, str(marker)],
        env=world.env,
        stderr=subprocess.PIPE,
        text=True,
    )
    pid = int(wait_for_file(Path(f"{marker}.pid")))
    wait_for_file(Path(f"{marker}.ready"))
    proc.send_signal(signal.SIGTERM)
    proc.wait(timeout=30)
    time.sleep(0.3)
    with pytest.raises(ProcessLookupError):
        os.kill(pid, 0)  # no orphan left holding the radio lock
    assert calls(world) == ["stop", "start"]


def test_the_commands_exit_status_survives_even_when_it_fails_instantly(world):
    assert borrow(world, "false").returncode == 1
    assert borrow(world, "definitely-not-a-command-xyz").returncode == 127
    assert borrow(world, "bash", "-c", "exit 9").returncode == 9


def test_a_missing_user_manager_is_an_error_not_a_reason_to_run_unprotected(world):
    (world.fake / "no-bus").touch()
    marker = world.tmp / "ran"
    result = borrow(world, "touch", str(marker))
    assert result.returncode == 125
    assert not marker.exists()
    assert "cannot reach the systemd user manager" in result.stderr


def test_an_unverified_restore_or_a_stale_status_is_called_out_loudly(world):
    run = world.run / "amateur-radio"
    run.mkdir(parents=True)
    (run / "receiver.json").write_text(
        json.dumps({"state": "waiting", "last_restore": "UNVERIFIED: no answer"})
    )
    result = borrow(world, "true")
    assert "could not verify" in result.stderr
    assert "not 'stopped'" in result.stderr


def test_a_stale_lock_held_flag_is_not_trusted(world, monkeypatch):
    monkeypatch.setattr(radio_lock, "LOCK_PATH", world.env["PLUTO_RADIO_LOCK"])
    monkeypatch.setenv("PLUTO_RADIO_LOCK_HELD", "1")
    # Nobody holds the lock, so the flag is ignored and the lock is really taken.
    with radio_lock.radio_lock(timeout_s=1):
        probe = subprocess.run(
            ["flock", "-n", world.env["PLUTO_RADIO_LOCK"], "true"], check=False, capture_output=True
        )
        assert probe.returncode != 0, "the lock must be held inside the block"


@pytest.mark.parametrize("script", ["capture_wideband.py"])
def test_every_radio_script_steps_the_live_service_aside_before_taking_the_lock(script):
    source = (BORROW.parent / script).read_text()
    assert "ensure_borrowed()" in source
    assert source.index("ensure_borrowed()") < source.index("radio_lock(")
