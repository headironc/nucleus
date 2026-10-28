import { expect, test } from "bun:test";

test("native maintenance-tools rejects a failing assertion", () => {
  expect("native-tools-actual").toBe("native-tools-expected");
});
