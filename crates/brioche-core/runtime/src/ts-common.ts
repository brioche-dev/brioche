import ts from "typescript";

export const TS_CONFIG = {
  strict: true,
  noFallthroughCasesInSwitch: true,
  noImplicitOverride: true,
  noImplicitReturns: true,
  noUncheckedIndexedAccess: true,
  module: ts.ModuleKind.ESNext,
  target: ts.ScriptTarget.ES2025,
  resolveJsonModule: true,
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

// Virtual declaration files backing `text` and `bytes` import attributes.
// They are keyed only by the attribute type, so every import of a given type
// shares one declaration.
const ATTRIBUTE_MODULE_PREFIX = "briocheattr:///";

const ATTRIBUTE_MODULE_SOURCES = {
  text: "declare const value: string;\nexport default value;\n",
  bytes: "declare const value: Uint8Array;\nexport default value;\n",
} as const;

type AttributeModuleType = keyof typeof ATTRIBUTE_MODULE_SOURCES;

// Read the `type` value from a module specifier's import attribute, if any.
export function importAttributeType(moduleLiteral: ts.StringLiteralLike): string | undefined {
  const parent = moduleLiteral.parent;
  const attributes = ts.isImportDeclaration(parent) || ts.isExportDeclaration(parent)
    ? parent.attributes
    : undefined;
  if (attributes == null) {
    return undefined;
  }

  for (const element of attributes.elements) {
    if (element.name.text === "type" && ts.isStringLiteralLike(element.value)) {
      return element.value.text;
    }
  }

  return undefined;
}

// Resolve an import from its attribute type. `json` points at the real file so
// `resolveJsonModule` can type it from its contents, while `text` and `bytes`
// resolve to a shared virtual declaration. Returns undefined for anything else,
// which falls back to normal file lookup.
export function resolveAttributeModule(attributeType: string, resolvedName: string): ts.ResolvedModuleFull | undefined {
  if (attributeType === "json") {
    return {
      extension: ts.Extension.Json,
      resolvedFileName: resolvedName,
    };
  } else if (attributeType in ATTRIBUTE_MODULE_SOURCES) {
    return {
      extension: ts.Extension.Dts,
      resolvedFileName: `${ATTRIBUTE_MODULE_PREFIX}${attributeType}.d.ts`,
    };
  }

  return undefined;
}

// Source text for a virtual attribute declaration file, or undefined if the
// name doesn't refer to one.
export function attributeModuleSource(fileName: string): string | undefined {
  if (!fileName.startsWith(ATTRIBUTE_MODULE_PREFIX)) {
    return undefined;
  }

  const type = fileName.slice(ATTRIBUTE_MODULE_PREFIX.length, -".d.ts".length);
  return ATTRIBUTE_MODULE_SOURCES[type as AttributeModuleType];
}
