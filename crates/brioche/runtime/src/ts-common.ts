import * as ts from "typescript";

export const TS_CONFIG = {
  strict: true,
  noFallthroughCasesInSwitch: true,
  noImplicitOverride: true,
  noImplicitReturns: true,
  noUncheckedIndexedAccess: true,
  module: ts.ModuleKind.ES2022,
  target: ts.ScriptTarget.ES2022,
} satisfies ts.CompilerOptions;

export const DEFAULT_LIB_URL = "briocheruntime:///tslib/lib.esnext.d.ts";

const ops = (globalThis as any).Deno.core.ops;

export function toTsUrl(url: string): string {
  if (url.startsWith("file://") && url.endsWith(".bri")) {
    return `${url}.ts`;
  } else {
    return url;
  }
}

export function fromTsUrl(url: string): string {
  if (url.startsWith("file://")) {
    return url.replace(/\.bri\.ts$/, ".bri");
  } else {
    return url;
  }
}

export function readFile(specifierUrl: string): string {
  return ops.op_brioche_file_read(specifierUrl);
}

export function resolveModule(specifier: string, referrer: string): string {
  return ops.op_brioche_resolve_module(specifier, referrer);
}

export function fileExists(specifierUrl: string): boolean {
  return ops.op_brioche_file_exists(specifierUrl);
}

export function fileVersion(specifierUrl: string): number | null | undefined {
  return ops.op_brioche_file_version(specifierUrl);
}
