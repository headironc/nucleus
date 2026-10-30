const labels = new Map<number, string>([
  [100, "临床"],
  [200, "护理"],
  [300, "医技"],
  [400, "药剂"],
  [500, "财务"],
  [600, "行政"],
  [700, "后勤"],
  [801, "第三方公司"],
  [802, "保安"],
  [803, "物业"],
  [804, "志愿者"],
  [901, "见习"],
  [902, "实习"],
  [903, "进修生"],
  [904, "学生"],
  [1000, "临时人员"],
]);

export const numberSubtypes = Array.from(labels, ([value, name]) => ({
  value,
  name,
}));

export type NumberSummary = Readonly<{ display: string; subtype: string }>;

/** Stored numbers keep their complete year; display text is never an identity. */
export function summarize(value: {
  subtype: number;
  year: number;
  serial: number;
}): NumberSummary | undefined {
  const subtype = labels.get(value.subtype);
  if (
    !subtype ||
    !Number.isInteger(value.year) ||
    value.year < 1 ||
    value.year > 2_147_483_647 ||
    !Number.isInteger(value.serial) ||
    value.serial < 1 ||
    value.serial > 999
  )
    return undefined;
  return {
    display: `${Math.floor(value.subtype / 100)}${String(value.year % 100).padStart(2, "0")}${String(value.serial).padStart(2, "0")}`,
    subtype,
  };
}
