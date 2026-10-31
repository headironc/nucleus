import { join } from "node:path";
import { packageOutput } from "../tools/package-output";

const output = await packageOutput();
export const packageDirectory = output.name;
export function packageFile(path: string) { return join(output.directory, path); }
