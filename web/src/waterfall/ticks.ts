/** Whole-second ages, placed against the actual timestamps of displayed rows. */
export function historyTicks(times: readonly number[], visibleRows: number, height: number) {
  if (!times.length || visibleRows <= 0 || height <= 0 || !Number.isFinite(times[0])) return [];
  const rows = times.slice(0, visibleRows);
  const maxAge = times[0] - rows.at(-1)!;
  const ticks = [{ ageSeconds: 0, row: 0 }];
  if (!Number.isFinite(maxAge) || maxAge < 1) return ticks;
  const rawStep = Math.max(1, maxAge / Math.max(1, Math.floor(height / 90)));
  const magnitude = 10 ** Math.floor(Math.log10(rawStep));
  const step = [1, 2, 5, 10].find((n) => n * magnitude >= rawStep)! * magnitude;
  let row = 1;
  for (let ageSeconds = step; ageSeconds <= maxAge; ageSeconds += step) {
    while (row < rows.length && times[0] - rows[row] < ageSeconds) row++;
    if (row >= rows.length) break;
    const before = times[0] - rows[row - 1];
    const after = times[0] - rows[row];
    if (!Number.isFinite(before) || !Number.isFinite(after) || after <= before) continue;
    const position = row - 1 + (ageSeconds - before) / (after - before);
    // Bursty rows or timestamp gaps can otherwise pile several labels together.
    if ((position - ticks.at(-1)!.row) / visibleRows * height >= 60)
      ticks.push({ ageSeconds, row: position });
  }
  return ticks;
}
