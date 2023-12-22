import * as ts from "typescript";
import { TS_CONFIG, DEFAULT_LIB_URL, toTsUrl, fromTsUrl, readFile, fileExists, resolveModule } from "./tscommon.ts";

export function check(files: string[]): Diagnostic[] {
  const firstFile = files.at(0);
  const workingDir = firstFile != null ? firstFile.split("/").slice(0, -1).join("/") : "file:///";

  const host = new BriocheCompilerHost(workingDir) satisfies ts.CompilerHost;

  const program = ts.createProgram(files.map(toTsUrl), TS_CONFIG, host);

  const diagnostics = program.getSemanticDiagnostics();
  return serializeDiagnostics(diagnostics);
}

function serializeDiagnostics(diagnostics: readonly ts.Diagnostic[]): Diagnostic[] {
  return diagnostics.map((diag) => ({
    specifier: diag.file != null ? fromTsUrl(diag.file.fileName) : undefined,
    start: diag.start,
    length: diag.length,
    message: serializeChain(diag.messageText, diag.category),
  } satisfies Diagnostic))
}

function serializeChain(message: string | ts.DiagnosticMessageChain, category: ts.DiagnosticCategory): DiagnosticMessage {
  if (typeof message === "string") {
    return {
      level: level(category),
      text: message,
      nested: [],
    };
  }

  const next = message.next ?? [];

  return {
    level: level(category),
    text: message.messageText,
    nested: next.map((chain) => serializeChain(chain, chain.category)),
  }
}

function level(category: ts.DiagnosticCategory): DiagnosticLevel {
  switch (category) {
    case ts.DiagnosticCategory.Message:
      return "message";
    case ts.DiagnosticCategory.Suggestion:
      return "suggestion";
    case ts.DiagnosticCategory.Warning:
      return "warning";
    case ts.DiagnosticCategory.Error:
      return "error";
  }
}

interface Diagnostic {
  specifier?: string,
  start?: number,
  length?: number,
  message: DiagnosticMessage,
}

interface DiagnosticMessage {
  level: DiagnosticLevel,
  text: string,
  nested: DiagnosticMessage[],
}

type DiagnosticLevel = "message" | "suggestion" | "warning" | "error";

class BriocheCompilerHost implements ts.CompilerHost {
  workingDir: string;

  constructor(workingDir: string) {
    this.workingDir = workingDir;
  }

  getSourceFile(fileName: string, languageVersionOrOptions: ts.ScriptTarget | ts.CreateSourceFileOptions): ts.SourceFile | undefined {
    const sourceText = this.readFile(fileName);
    if (sourceText == null) {
      return undefined;
    }
    return ts.createSourceFile(fileName, sourceText, languageVersionOrOptions, false, ts.ScriptKind.TS);
  }
  getDefaultLibFileName(options: ts.CompilerOptions): string {
    return DEFAULT_LIB_URL;
  }
  writeFile: ts.WriteFileCallback = (fileName) => {
    throw new Error(`Method not implemented: writeFile(${fileName}, ...)`);
  }
  getCurrentDirectory(): string {
    return this.workingDir;
  }
  getCanonicalFileName(fileName: string): string {
    return fileName;
  }
  useCaseSensitiveFileNames(): boolean {
    return true;
  }
  getNewLine(): string {
    return "\n";
  }
  fileExists(fileName: string): boolean {
    return fileExists(fromTsUrl(fileName));
  }
  readFile(fileName: string): string | undefined {
    return readFile(fromTsUrl(fileName));
  }
  resolveModuleNameLiterals(moduleLiterals: readonly ts.StringLiteralLike[], containingFile: string): readonly ts.ResolvedModuleWithFailedLookupLocations[] {
    return moduleLiterals.map(moduleLiteral => {

      const resolvedName = resolveModule(moduleLiteral.text, fromTsUrl(containingFile));

      if (resolvedName != null) {
        return {
          resolvedModule: {
            extension: ".ts",
            resolvedFileName: toTsUrl(resolvedName),
          }
        }
      } else {
        return {
          resolvedModule: undefined,
        };
      }
    });
  }
}
