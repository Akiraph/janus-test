export interface ShellCommandPolicy {
  readonly compressible: boolean;
}

export const safeShellCommandExamples = [
  "pwd",
  "ls",
  "cat README.md",
  "cat /workspace/README.md",
  "head README.md",
  "tail README.md",
  "ls src/*.ts",
  "ls -d */",
  "cd . && pwd",
  "rg Workspace",
  "rg Workspace /workspace/apps | wc -l",
  "grep Workspace README.md",
  "wc -l README.md",
  "file README.md",
  "stat README.md",
  "du -sh .",
  "df -h",
  "git status",
  "git status && git diff --stat",
  "git diff",
  "git log",
  "git show HEAD",
  "bun test",
  "bun run typecheck",
  "npm run lint",
  "pnpm test",
  "yarn run build",
  "node scripts/check.js",
  "tsc --noEmit",
  "vitest run",
] as const;

const safeStandaloneCommands = new Set([
  "pwd",
  "ls",
  "cat",
  "head",
  "tail",
  "dirname",
  "basename",
  "which",
  "file",
  "stat",
  "du",
  "df",
  "grep",
  "wc",
  "sort",
  "uniq",
  "cut",
  "rg",
  "echo",
  "printf",
  "date",
  "whoami",
  "id",
  "uname",
  "find",
  "test",
  "true",
  "false",
  "node",
  "tsx",
  "ts-node",
  "tsc",
  "vite",
  "vitest",
  "eslint",
  "biome",
  "prettier",
]);

const safeGitSubcommands = new Set([
  "status",
  "diff",
  "log",
  "show",
  "branch",
  "rev-parse",
  "ls-files",
  "grep",
  "describe",
  "remote",
]);

const safePackageRunnerSubcommands = new Set(["run", "test"]);

const hardDeniedCommands = new Set([
  "sudo",
  "su",
  "doas",
  "env",
  "printenv",
  "set",
  "export",
  "unset",
  "docker",
  "podman",
  "kubectl",
  "ssh",
  "scp",
  "sftp",
  "rsync",
  "xargs",
  "powershell",
  "pwsh",
  "cmd",
  "bash",
  "sh",
  "zsh",
  "fish",
  "dash",
  "ash",
  "exec",
  "trap",
  "eval",
  "command",
  "builtin",
  "enable",
  "alias",
  "unalias",
  "source",
  ".",
]);

const hardDeniedGitSubcommands = new Set([
  "clean",
  "push",
  "reset",
  "checkout",
  "switch",
  "restore",
  "rebase",
  "merge",
  "commit",
]);

export function classifyShellCommand(command: string): ShellCommandPolicy {
  return {
    compressible: isCompressibleShellCommand(command),
  };
}

function isCompressibleShellCommand(command: string): boolean {
  const tokens = shellCommandTokens(command);

  return (
    tokens !== undefined &&
    tokens.length > 0 &&
    !hasCompressibleDeniedShellSyntax(command) &&
    !isHardDeniedTokenizedCommand(command, tokens) &&
    isCompressibleTokenizedCommand(tokens)
  );
}

export function isHardDeniedShellCommand(command: string): boolean {
  const tokens = shellCommandTokens(command);

  return (
    tokens === undefined ||
    tokens.length === 0 ||
    isHardDeniedTokenizedCommand(command, tokens)
  );
}

const forbiddenOptions = new Set([
  "--cwd",
  "--git-dir",
  "--work-tree",
  "--prefix",
]);

const credentialPathSegments = new Set([
  ".env",
  ".npmrc",
  ".netrc",
  ".pypirc",
  "id_rsa",
  "id_dsa",
  "id_ecdsa",
  "id_ed25519",
]);

function shellCommandTokens(command: string): readonly string[] | undefined {
  const tokens: string[] = [];
  let token = "";
  let quote: "'" | '"' | undefined;

  for (let index = 0; index < command.length; index += 1) {
    const char = command[index];

    if (char === undefined) {
      continue;
    }

    if (quote === undefined && /\s/.test(char)) {
      if (token.length > 0) {
        tokens.push(token);
        token = "";
      }
      continue;
    }

    if (quote === undefined && isShellOperatorStart(command, index)) {
      if (token.length > 0) {
        tokens.push(token);
        token = "";
      }

      const operator = readShellOperator(command, index);
      tokens.push(operator.token);
      index = operator.nextIndex - 1;
      continue;
    }

    if ((char === "'" || char === '"') && quote === undefined) {
      quote = char;
      continue;
    }

    if (char === quote) {
      quote = undefined;
      continue;
    }

    token += char;
  }

  if (token.length > 0) {
    tokens.push(token);
  }

  if (quote !== undefined) {
    return undefined;
  }

  return tokens;
}

function isShellOperatorStart(command: string, index: number): boolean {
  const char = command[index];
  const next = command[index + 1];

  return (
    char === ";" ||
    char === "|" ||
    char === "&" ||
    char === "<" ||
    char === ">" ||
    (/\d/.test(char ?? "") && next === ">")
  );
}

function readShellOperator(
  command: string,
  index: number,
): { readonly token: string; readonly nextIndex: number } {
  const rest = command.slice(index);

  for (const operator of [
    "2>&1",
    "1>&2",
    "&&",
    "||",
    ">>",
    "<<",
    "2>>",
    "1>>",
    "2>",
    "1>",
    "&>>",
    "&>",
  ]) {
    if (rest.startsWith(operator)) {
      return { token: operator, nextIndex: index + operator.length };
    }
  }

  const char = command[index] ?? "";
  return { token: char, nextIndex: index + 1 };
}

function isUnsafeToken(token: string): boolean {
  const lower = token.toLowerCase();

  return (
    !isShellOperatorToken(token) &&
    ((token.startsWith("/") && !isWorkspaceAbsolutePath(token)) ||
      containsUnsafeAbsolutePath(token) ||
      token.startsWith("~") ||
      /^[A-Za-z]:[\\/]/.test(token) ||
      token.includes("\\") ||
      token === ".." ||
      token.startsWith("../") ||
      token.includes("/../") ||
      token.endsWith("/..") ||
      (token.startsWith("-C") && token !== "-C") ||
      forbiddenOptions.has(lower) ||
      [...forbiddenOptions].some(
        (option) => lower.startsWith(`${option}=`) || lower.startsWith(option),
      ) ||
      isCredentialPathToken(lower) ||
      isUnsafeUnquotedExpansionToken(token))
  );
}

function containsUnsafeAbsolutePath(token: string): boolean {
  const normalized = token.replaceAll("\\", "/");
  return /(^|[("'=,:])\/(?!workspace(?:\/|$))/.test(normalized);
}

function isCompressibleTokenizedCommand(tokens: readonly string[]): boolean {
  const segments = shellCommandSegments(tokens);

  return (
    segments?.every((segment) =>
      isCompressibleSimpleCommand(segment.map((token) => token.toLowerCase())),
    ) ?? false
  );
}

function isCompressibleSimpleCommand(tokens: readonly string[]): boolean {
  const executable = tokens[0];

  if (executable === undefined) {
    return false;
  }

  if (executable === "cd") {
    return tokens.length === 2 && isNoopWorkingDirectoryTarget(tokens[1]);
  }

  if (safeStandaloneCommands.has(executable)) {
    return true;
  }

  if (executable === "git") {
    const subcommand = gitSubcommand(tokens);
    return subcommand !== undefined && safeGitSubcommands.has(subcommand);
  }

  if (executable === "bun") {
    const subcommand = tokens[1];
    return (
      subcommand === undefined || safePackageRunnerSubcommands.has(subcommand)
    );
  }

  if (executable === "npm" || executable === "pnpm" || executable === "yarn") {
    const subcommand = tokens[1];
    return (
      subcommand !== undefined && safePackageRunnerSubcommands.has(subcommand)
    );
  }

  return false;
}

function isHardDeniedTokenizedCommand(
  command: string,
  tokens: readonly string[],
): boolean {
  const segments = shellCommandSegments(tokens);

  if (
    hasHardDeniedShellSyntax(command) ||
    tokens.some(isUnsafeToken) ||
    segments === undefined ||
    segments.some(isHardDeniedSimpleCommand)
  ) {
    return true;
  }

  return false;
}

function isHardDeniedSimpleCommand(tokens: readonly string[]): boolean {
  const normalizedTokens = tokens.map((token) => token.toLowerCase());
  const executable = normalizedTokens[0];

  if (executable === undefined) {
    return true;
  }

  if (hardDeniedCommands.has(executable)) {
    return true;
  }

  if (executable === "rm" || executable === "rmdir") {
    return isHardDeniedRemoveCommand(normalizedTokens);
  }

  if (executable === "find") {
    return (
      normalizedTokens.includes("-delete") || normalizedTokens.includes("-exec")
    );
  }

  if (executable === "git") {
    if (!hasValidGitWorkingDirectoryOption(normalizedTokens)) {
      return true;
    }

    const subcommand = gitSubcommand(normalizedTokens);

    if (subcommand !== undefined && hardDeniedGitSubcommands.has(subcommand)) {
      return true;
    }
  }

  return false;
}

function gitSubcommand(tokens: readonly string[]): string | undefined {
  for (let index = 1; index < tokens.length; index += 1) {
    const token = tokens[index];

    if (token === "-c") {
      index += 1;
      continue;
    }

    return token;
  }

  return undefined;
}

function hasValidGitWorkingDirectoryOption(tokens: readonly string[]): boolean {
  for (let index = 1; index < tokens.length; index += 1) {
    const token = tokens[index];

    if (token !== "-c") {
      continue;
    }

    const target = tokens[index + 1];
    if (target === undefined || !isWorkspaceContainedPathToken(target)) {
      return false;
    }

    index += 1;
  }

  return true;
}

function isHardDeniedRemoveCommand(tokens: readonly string[]): boolean {
  const targets = tokens.slice(1).filter((token) => !isRemoveOption(token));

  return (
    targets.length === 0 ||
    targets.some(
      (target) =>
        !isWorkspaceContainedPathToken(target) ||
        isBroadDeleteTarget(target) ||
        isProtectedWorkspaceMutationTarget(target),
    )
  );
}

function isRemoveOption(token: string): boolean {
  return token.startsWith("-");
}

function isWorkspaceContainedPathToken(token: string): boolean {
  return (
    token === "." ||
    token === "./" ||
    token.startsWith("./") ||
    (!token.startsWith("/") &&
      token !== ".." &&
      !token.startsWith("../") &&
      !token.includes("/../") &&
      !token.endsWith("/..")) ||
    isWorkspaceAbsolutePath(token)
  );
}

function isBroadDeleteTarget(token: string): boolean {
  const normalized = token.replaceAll("\\", "/").replace(/\/+$/, "");
  return (
    normalized === "." ||
    normalized === "/workspace" ||
    normalized === "*" ||
    normalized === "./*" ||
    normalized === "/workspace/*"
  );
}

function isProtectedWorkspaceMutationTarget(token: string): boolean {
  const parts = token.replaceAll("\\", "/").split("/").filter(Boolean);
  return parts.includes(".git") || parts.some(isCredentialPathPart);
}

function shellCommandSegments(
  tokens: readonly string[],
): readonly (readonly string[])[] | undefined {
  const segments: string[][] = [];
  let segment: string[] = [];

  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];

    if (token === undefined) {
      continue;
    }

    if (isBackgroundOperator(token) || token === "<<") {
      return undefined;
    }

    if (isControlOperator(token)) {
      if (segment.length === 0) {
        return undefined;
      }

      segments.push(segment);
      segment = [];
      continue;
    }

    if (isRedirectionOperator(token)) {
      if (token === "2>&1" || token === "1>&2") {
        continue;
      }

      const target = tokens[index + 1];
      if (target === undefined || isShellOperatorToken(target)) {
        return undefined;
      }

      index += 1;
      continue;
    }

    segment.push(token);
  }

  if (segment.length === 0) {
    return segments.length > 0 ? undefined : [];
  }

  segments.push(segment);
  return segments;
}

function isWorkspaceAbsolutePath(token: string): boolean {
  return token === "/workspace" || token.startsWith("/workspace/");
}

function isNoopWorkingDirectoryTarget(token: string | undefined): boolean {
  return (
    token === "." ||
    token === "./" ||
    token === "/workspace" ||
    token === "/workspace/"
  );
}

function isShellOperatorToken(token: string): boolean {
  return (
    isControlOperator(token) ||
    isRedirectionOperator(token) ||
    isBackgroundOperator(token)
  );
}

function isControlOperator(token: string): boolean {
  return token === "&&" || token === "||" || token === ";" || token === "|";
}

function isRedirectionOperator(token: string): boolean {
  return (
    token === ">" ||
    token === ">>" ||
    token === "<" ||
    token === "1>" ||
    token === "1>>" ||
    token === "2>" ||
    token === "2>>" ||
    token === "&>" ||
    token === "&>>" ||
    token === "2>&1" ||
    token === "1>&2"
  );
}

function isBackgroundOperator(token: string): boolean {
  return token === "&";
}

function isCredentialPathToken(token: string): boolean {
  const normalized = token.replaceAll("\\", "/");
  const parts = normalized.split("/").filter(Boolean);

  return (
    parts.some(isCredentialPathPart) ||
    normalized.includes("/.ssh/") ||
    normalized.startsWith(".ssh/")
  );
}

function isCredentialPathPart(part: string): boolean {
  return (
    credentialPathSegments.has(part) ||
    part === ".ssh" ||
    part.startsWith(".env")
  );
}

function isUnsafeUnquotedExpansionToken(token: string): boolean {
  return token.includes("[") || token.includes("]") || token.includes("{");
}

function hasHardDeniedShellSyntax(command: string): boolean {
  return /[`\n\r]/.test(command) || /\$(?:[{(]|\w)/.test(command);
}

function hasCompressibleDeniedShellSyntax(command: string): boolean {
  return /[{}[\]]/.test(command);
}
