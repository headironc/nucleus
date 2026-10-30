// Controlled command substitutes for bootstrap.test.ts. No network or sudo calls.
import {
  appendFileSync,
  copyFileSync,
  mkdirSync,
  readFileSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";

const root = process.env.PROBE_FIXTURE!;
const mode = process.env.PROBE_CASE ?? "success";
const [command, ...args] = process.argv.slice(2);
const runner = join(root, "home/runner/actions-runner");
const service = "actions.runner.nucleus-fixture.service";
function record(value: object) {
  appendFileSync(join(root, "calls.jsonl"), JSON.stringify(value) + "\n");
}
function safePath(path: string) {
  if (!resolve(path).startsWith(root + "/"))
    throw new Error("Substitute refused a path outside its fixture");
  return path;
}
function fail() {
  process.exit(22);
}

switch (command) {
  case "curl": {
    const url = new URL(args.at(-1)!);
    if (
      url.origin !== "https://10.203.64.1:9444" ||
      !args.includes("@-") ||
      args.some((arg) => arg.includes("fixture.secret.token")) ||
      readFileSync(0, "utf8").trim() !==
        "Authorization: Bearer fixture.secret.token"
    )
      throw new Error("Unexpected metadata transport or credential channel");
    const path = url.pathname + url.search;
    if (path.startsWith("/api/v1/callbacks/")) {
      const body = JSON.parse(readFileSync(3, "utf8")) as {
        status?: string;
        message?: string;
        agent_id: number | null;
      };
      record({ command, path, body });
      if (mode === "callback-failure") fail();
      if (
        mode === "identity-race" &&
        body.message === "Installing instance JIT identity"
      )
        symlinkSync(join(root, "untouched"), join(runner, ".credentials"));
      break;
    }
    record({ command, path });
    if (mode === "metadata-failure" && path.endsWith("/credentials")) fail();
    const values: Record<string, string> = {
      "/api/v1/metadata/credentials/runner": '{"AgentId":123456}',
      "/api/v1/metadata/credentials/credentials": "synthetic-jit-credential",
      "/api/v1/metadata/credentials/credentials_rsaparams": "synthetic-rsa",
      "/api/v1/metadata/system/service-name": service.slice(0, -8),
      "/api/v1/metadata/systemd/unit-file?runAsUser=runner":
        "[Service]\nUser=runner\n",
    };
    if (!(path in values)) throw new Error("Unexpected metadata resource");
    process.stdout.write(values[path]! + "\n");
    break;
  }
  case "docker": {
    record({ command, args });
    switch (args.join(" ")) {
      case "version --format {{.Client.Version}} {{.Server.Version}}":
        console.log(mode === "wrong-version" ? "0.0.0 0.0.0" : "29.1.3 29.1.3");
        break;
      case "context show":
        console.log(mode === "wrong-context" ? "remote" : "default");
        break;
      case "context inspect default":
        console.log(
          JSON.stringify([
            {
              Name: "default",
              Endpoints: {
                docker: {
                  Host:
                    mode === "remote-docker"
                      ? "tcp://example.invalid:2375"
                      : "unix:///var/run/docker.sock",
                },
              },
            },
          ]),
        );
        break;
      case "image ls --all --quiet":
        if (mode === "warm-images") console.log("synthetic-image-id");
        break;
      case "container ls --all --quiet":
        if (mode === "existing-container")
          console.log("synthetic-container-id");
        break;
      default:
        throw new Error("Unexpected Docker operation");
    }
    break;
  }
  case "sudo": {
    if (args.shift() !== "-n") throw new Error("Interactive sudo is forbidden");
    record({ command, args });
    if (args[0] === "systemctl") {
      if (args.join(" ") === "systemctl is-active --quiet rsyslog.service")
        process.exit(mode === "collector-active" ? 0 : 3);
      if (args[1] === "start" && mode === "service-failure") fail();
      const allowed = [
        "systemctl disable --now rsyslog.service",
        "systemctl daemon-reload",
        `systemctl enable ${service}`,
        `systemctl start ${service}`,
        `systemctl is-active --quiet ${service}`,
      ];
      if (!allowed.includes(args.join(" ")))
        throw new Error("Unexpected service operation");
    } else if (args[0] === "install") {
      const destination = safePath(args.at(-1)!);
      if (args.includes("-d")) mkdirSync(destination, { mode: 0o755 });
      else if (args.at(-2) === "/dev/stdin")
        writeFileSync(destination, readFileSync(0), { mode: 0o644 });
      else copyFileSync(safePath(args.at(-2)!), destination);
    } else throw new Error("Unexpected privileged operation");
    break;
  }
  default:
    throw new Error("Unexpected substitute command");
}
