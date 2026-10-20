import { z } from "zod";

/** Lenient list: one malformed item must not drop the others. */
export function lenientArray<T>(item: z.ZodType<T>) {
  return z
    .array(z.unknown())
    .catch([])
    .transform((items) =>
      items.flatMap((raw) => {
        const parsed = item.safeParse(raw);
        return parsed.success ? [parsed.data] : [];
      }),
    );
}
