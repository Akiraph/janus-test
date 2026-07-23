import { describe, expect, test } from "bun:test";
import fc from "fast-check";
import {
  classifyShellCommand,
  isHardDeniedShellCommand,
  safeShellCommandExamples,
} from "./shell-policy";

const propertyOptions = { numRuns: 150, seed: 20260627 };
const safePathChars = [
  "a",
  "b",
  "c",
  "d",
  "e",
  "f",
  "i",
  "j",
  "n",
  "o",
  "p",
  "r",
  "s",
  "t",
  "u",
  "x",
  "y",
  "z",
  "0",
  "1",
  "2",
  "3",
  "-",
  "_",
  ".",
] as const;
const safePathSegmentArbitrary = fc
  .array(fc.constantFrom(...safePathChars), {
    minLength: 1,
    maxLength: 10,
  })
  .map((chars) => chars.join(""))
  .filter(
    (segment) =>
      segment !== "." &&
      segment !== ".." &&
      segment !== ".ssh" &&
      !segment.startsWith(".env") &&
      !segment.startsWith("id_"),
  );
const safeRelativePathArbitrary = fc
  .array(safePathSegmentArbitrary, { minLength: 1, maxLength: 4 })
  .map((segments) => segments.join("/"));
const safeFileCommandArbitrary = fc
  .tuple(
    fc.constantFrom("cat", "head", "tail", "file", "stat", "rg", "grep", "wc"),
    safeRelativePathArbitrary,
  )
  .map(([executable, path]) => `${executable} ${path}`);
const safeCommandArbitrary = fc.oneof(
  fc.constantFrom("pwd", "date", "whoami", "git status --short", "bun test"),
  safeFileCommandArbitrary,
);
const unsafePathArbitrary = fc.oneof(
  safeRelativePathArbitrary.map((path) => `../${path}`),
  safeRelativePathArbitrary.map((path) => `/etc/${path}`),
  safeRelativePathArbitrary.map((path) => `src\\${path}`),
  fc.constantFrom(
    ".env",
    ".env.local",
    ".ssh/id_ed25519",
    "~/.ssh/config",
    "/workspace/../secret",
    "C:/Users/Administrator/secret.txt",
  ),
);

describe("classifyShellCommand", () => {
  test("marks configured read-only commands as compressible", () => {
    for (const command of [
      "pwd",
      "ls -la",
      "cat README.md",
      "cat /workspace/README.md",
      "head -20 package.json",
      "tail -20 server.log",
      "ls src/*.ts",
      "ls -d */",
      "cd . && pwd",
      "cd /workspace && pwd",
      "dirname apps/web/package.json",
      "basename apps/web/package.json",
      "which bash",
      "file README.md",
      "stat README.md",
      "du -sh .",
      "df -h",
      "grep -R Janus README.md",
      "wc -l README.md",
      "sort package.json",
      "uniq names.txt",
      "cut -d: -f1 package.json",
      "rg Workspace",
      "rg *.ts",
      "rg Workspace /workspace/apps | wc -l",
      "echo hello",
      "echo hello > /workspace/output.txt",
      "printf hello",
      "date",
      "whoami",
      "id",
      "uname -a",
      "git status --short",
      "git -C . status --short",
      "git -C src status --short",
      "git -C /workspace status --short",
      "git status --short && git diff --stat",
      "git status; git diff --stat",
      "git diff --stat",
      "git log --oneline -5",
      "git show HEAD",
      "bun test apps/server/src/example.test.ts",
      "bun run typecheck",
      "npm run lint",
      "pnpm test apps/web",
      "yarn run build",
      "node scripts/check.js",
      "tsc --noEmit",
      "vitest run apps/server",
      "find apps -maxdepth 2 -type f",
    ]) {
      expect(classifyShellCommand(command)).toEqual({
        compressible: true,
      });
    }
  });

  test("does not mark unsafe shell command lines as compressible", () => {
    for (const command of [
      "cd src",
      "printenv PATH",
      "sleep 1",
      "docker compose logs",
      "ls && rm -rf dist",
      "echo secret > .env",
      "echo secret >/workspace/.env",
      "cat /workspace/../secret",
      "echo $JANUS_ACCESS_TOKEN",
      "cat '/etc/passwd'",
      "python -c \"open('/etc/passwd').read()\"",
      "sh -lc pwd",
      "bash scripts/check.sh",
      "exec pwd",
      "trap true EXIT",
      'eval "trap - DEBUG; trap - EXIT"',
      "command trap - DEBUG",
      "builtin trap - EXIT",
      "enable -n trap",
      "alias cd=false",
      "source scripts/setup.sh",
      "git status; git clean -fd",
      "git clean -fd",
      "git reset --hard",
      "git -C .. status",
      "git -C.. status",
      "rm -rf .git",
      "rm -rf /workspace/.git",
      "rm -rf .",
      "rm -rf *",
      "find . -delete",
      "rg token | xargs rm",
      "cat $(pwd)/README.md",
      "cat ../AGENTS.md",
      "cat /etc/passwd",
      "cat /tmp/output.txt",
      "cat '/var/log/syslog'",
      "cat C:/Users/Administrator/Documents/Projects/Janus/AGENTS.md",
      "cat .env",
      "cat .env*",
      "cat .ssh/id_ed25519",
      "cd src",
      "cd /tmp",
      'echo "unterminated',
    ]) {
      expect(classifyShellCommand(command)).toEqual({
        compressible: false,
      });
    }
  });

  test("keeps model-facing examples inside the safe command policy", () => {
    for (const command of safeShellCommandExamples) {
      expect(classifyShellCommand(command)).toEqual({
        compressible: true,
      });
    }
  });

  test("accepts generated safe project-local command lines", () => {
    fc.assert(
      fc.property(safeCommandArbitrary, (command) => {
        expect(classifyShellCommand(command)).toEqual({
          compressible: true,
        });
      }),
      propertyOptions,
    );
  });

  test("rejects generated host, credential, and traversal path targets", () => {
    fc.assert(
      fc.property(unsafePathArbitrary, (path) => {
        expect(classifyShellCommand(`cat ${path}`)).toEqual({
          compressible: false,
        });
      }),
      propertyOptions,
    );
  });

  test("rejects every generated command classified as hard denied", () => {
    fc.assert(
      fc.property(fc.string({ maxLength: 120 }), (command) => {
        if (isHardDeniedShellCommand(command)) {
          expect(classifyShellCommand(command)).toEqual({
            compressible: false,
          });
        }
      }),
      propertyOptions,
    );
  });

  test("keeps quoted and unquoted safe local paths equivalent", () => {
    fc.assert(
      fc.property(
        fc.constantFrom("cat", "head", "tail", "file", "stat"),
        safeRelativePathArbitrary,
        (executable, path) => {
          expect(classifyShellCommand(`${executable} "${path}"`)).toEqual(
            classifyShellCommand(`${executable} ${path}`),
          );
        },
      ),
      propertyOptions,
    );
  });
});
