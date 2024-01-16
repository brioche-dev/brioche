import * as brioche from "./tscommon";
import * as ts from "typescript";
import * as lsp from "vscode-languageserver";
import { Linter } from "eslint";

export function buildLsp(): Lsp {
  return new Lsp();
}

class BriocheLanguageServiceHost implements ts.LanguageServiceHost {
  version = 0;
  files: Set<string> = new Set();

  getScriptFileNames(): string[] {
    return Array.from(this.files).map(brioche.toTsUrl);
  }

  getScriptVersion(fileName: string): string {
    const version = brioche.fileVersion(brioche.fromTsUrl(fileName)) ?? -1;
    return version.toString();
  }

  getCompilationSettings(): ts.CompilerOptions {
    return brioche.TS_CONFIG;
  }

  getScriptSnapshot(fileName: string): ts.IScriptSnapshot | undefined {
    const file = this.readFile(fileName);
    if (file == null) {
      return undefined;
    }

    return ts.ScriptSnapshot.fromString(file);
  }

  getCurrentDirectory(): string {
    return "file:///";
  }

  getDefaultLibFileName(options: ts.CompilerOptions): string {
    return brioche.DEFAULT_LIB_URL;
  }

  fileExists(fileName: string): boolean {
    return brioche.fileExists(brioche.fromTsUrl(fileName));
  }

  readFile(fileName: string): string | undefined {
    return brioche.readFile(brioche.fromTsUrl(fileName));
  }

  getSourceFile(fileName: string): ts.SourceFile {
    const snapshot = this.getScriptSnapshot(fileName);
    if (!snapshot) {
      throw new Error(`Unable to get script snapshot for file: ${fileName}`);
    }
    const text = snapshot.getText(0, snapshot.getLength());
    return ts.createSourceFile(fileName, text, ts.ScriptTarget.Latest, true);
  }

  resolveModuleNameLiterals(moduleLiterals: readonly ts.StringLiteralLike[], containingFile: string): readonly ts.ResolvedModuleWithFailedLookupLocations[] {
    return moduleLiterals.map(moduleLiteral => {
      const resolvedName = brioche.resolveModule(moduleLiteral.text, brioche.fromTsUrl(containingFile));

      if (resolvedName != null) {
        return {
          resolvedModule: {
            extension: ".ts",
            resolvedFileName: brioche.toTsUrl(resolvedName),
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

class Lsp {
  private host: BriocheLanguageServiceHost;
  private languageService: ts.LanguageService;
  private linter: Linter;

  constructor() {
    this.host = new BriocheLanguageServiceHost();
    const servicesHost: ts.LanguageServiceHost = this.host;
    this.languageService = ts.createLanguageService(servicesHost);
    this.linter = new Linter();
  }

  completion(params: lsp.TextDocumentPositionParams): lsp.CompletionItem[] {
    const fileName = params.textDocument.uri;
    const position = ts.getPositionOfLineAndCharacter(
      this.host.getSourceFile(fileName),
      params.position.line,
      params.position.character
    );

    const completions = this.languageService.getCompletionsAtPosition(brioche.toTsUrl(fileName), position, {});

    if (completions == null) {
      return [];
    }

    return completions.entries.map((entry) => {
      const item: lsp.CompletionItem = {
        label: entry.name,
      };

      if (entry.replacementSpan) {
        item.textEdit = {
          range: {
            start: this.host.getSourceFile(fileName).getLineAndCharacterOfPosition(entry.replacementSpan.start),
            end: this.host.getSourceFile(fileName).getLineAndCharacterOfPosition(entry.replacementSpan.start + entry.replacementSpan.length),
          },
          newText: entry.name,
        };
      }

      return item;
    });
  }

  diagnostic(params: lsp.DocumentDiagnosticParams): lsp.Diagnostic[] {
    const fileName = params.textDocument.uri;
    this.host.files.add(fileName);

    const sourceFile = this.host.getSourceFile(fileName);
    const tsLangaugeDiagnostics = this.languageService.getSemanticDiagnostics(brioche.toTsUrl(fileName));

    const tsDiagnostics = tsLangaugeDiagnostics.flatMap((diagnostic): lsp.Diagnostic[] => {
      if (diagnostic.start == null || diagnostic.length == null) {
        return [];
      }

      return [{
        range: {
          start: sourceFile.getLineAndCharacterOfPosition(diagnostic.start),
          end: sourceFile.getLineAndCharacterOfPosition(
            diagnostic.start + diagnostic.length
          ),
        },
        message: ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n"),
      }];
    });

    const eslintDiagnostics = this.linter.verify(sourceFile.getText(), {
      rules: {
        "no-unused-vars": "error",
      },
      parserOptions: {
        ecmaVersion: 2020,
        sourceType: "module",
      },
    });
    const lintDiagnostics = eslintDiagnostics.flatMap((diagnostic): lsp.Diagnostic[] => {
      const endLine = diagnostic.endLine ?? diagnostic.line;
      const endColumn = diagnostic.endColumn ?? diagnostic.column + 1;
      return [{
        range: {
          start: {
            line: diagnostic.line - 1,
            character: diagnostic.column,
          },
          end: {
            line: endLine - 1,
            character: endColumn,
          },
        },
        message: diagnostic.message,
        severity: lsp.DiagnosticSeverity.Warning,
      }]
    });


    return [...tsDiagnostics, ...lintDiagnostics];
  }

  gotoDefinition(params: lsp.TextDocumentPositionParams): lsp.Location | null {
    const fileName = params.textDocument.uri;
    const sourceFile = this.host.getSourceFile(fileName);
    if (sourceFile == null) {
      return null;
    }

    const position = ts.getPositionOfLineAndCharacter(
      sourceFile,
      params.position.line,
      params.position.character,
    );
    const definition = this.languageService.getDefinitionAtPosition(
      brioche.toTsUrl(fileName),
      position,
    );
    const def = definition?.[0];
    if (!def) {
      return null;
    }

    const defSourceFile = this.host.getSourceFile(def.fileName);
    if (defSourceFile == null) {
      return null;
    }

    return {
      uri: brioche.fromTsUrl(def.fileName),
      range: {
        start: ts.getLineAndCharacterOfPosition(defSourceFile, def.textSpan.start),
        end: ts.getLineAndCharacterOfPosition(defSourceFile, ts.textSpanEnd(def.textSpan)),
      },
    };
  }

  hover(params: lsp.TextDocumentPositionParams): lsp.Hover | null {
    const fileName = params.textDocument.uri;
    const sourceFile = this.host.getSourceFile(fileName);
    if (sourceFile == null) {
      return null;
    }

    const position = ts.getPositionOfLineAndCharacter(
      sourceFile,
      params.position.line,
      params.position.character,
    );
    const info = this.languageService.getQuickInfoAtPosition(brioche.toTsUrl(fileName), position);
    if (!info) {
      return null;
    }
    return {
      contents: ts.displayPartsToString(info.displayParts),
      range: {
        start: ts.getLineAndCharacterOfPosition(sourceFile, info.textSpan.start),
        end: ts.getLineAndCharacterOfPosition(sourceFile, ts.textSpanEnd(info.textSpan)),
      },
    };
  }

  references(params: lsp.ReferenceParams): lsp.Location[] | null {
    const fileName = params.textDocument.uri;
    const sourceFile = this.host.getSourceFile(fileName);
    if (sourceFile == null) {
      return null;
    }

    const position = ts.getPositionOfLineAndCharacter(
      sourceFile,
      params.position.line,
      params.position.character,
    );
    const references = this.languageService.getReferencesAtPosition(brioche.toTsUrl(fileName), position);
    if (!references) {
      return null;
    }
    return references.flatMap((ref) => {
      const refSourceFile = this.host.getSourceFile(ref.fileName);
      if (refSourceFile == null) {
        return [];
      }

      return [{
        uri: brioche.fromTsUrl(ref.fileName),
        range: {
          start: ts.getLineAndCharacterOfPosition(refSourceFile, ref.textSpan.start),
          end: ts.getLineAndCharacterOfPosition(refSourceFile, ts.textSpanEnd(ref.textSpan)),
        },
      }];
    });
  }

  documentHighlight(params: lsp.TextDocumentPositionParams): lsp.DocumentHighlight[] | null {
    const fileName = params.textDocument.uri;
    const sourceFile = this.host.getSourceFile(fileName);
    if (sourceFile == null) {
      return null;
    }

    const position = ts.getPositionOfLineAndCharacter(
      sourceFile,
      params.position.line,
      params.position.character,
    );
    const searchFilenames = new Set([...this.host.getScriptFileNames(), brioche.toTsUrl(fileName)]);
    const highlights = this.languageService.getDocumentHighlights(brioche.toTsUrl(fileName), position, Array.from(searchFilenames));
    if (!highlights) {
      return null;
    }

    return highlights.flatMap((highlight) => {
      const highlightSourceFile = this.host.getSourceFile(highlight.fileName);
      if (highlightSourceFile == null) {
        return [];
      }

      return highlight.highlightSpans.map((span) => {
        return {
          range: {
            start: ts.getLineAndCharacterOfPosition(highlightSourceFile, span.textSpan.start),
            end: ts.getLineAndCharacterOfPosition(highlightSourceFile, ts.textSpanEnd(span.textSpan)),
          },
        };
      });
    });
  }

  prepareRename(params: lsp.TextDocumentPositionParams): lsp.PrepareRenameResult | null {
    const fileName = params.textDocument.uri;
    const sourceFile = this.host.getSourceFile(fileName);
    if (sourceFile == null) {
      return null;
    }

    const position = ts.getPositionOfLineAndCharacter(
      sourceFile,
      params.position.line,
      params.position.character,
    );
    const rename = this.languageService.getRenameInfo(brioche.toTsUrl(fileName), position, {});
    if (rename == null || !rename.canRename) {
      return null;
    }

    return {
      placeholder: rename.displayName,
      range: {
        start: ts.getLineAndCharacterOfPosition(sourceFile, rename.triggerSpan.start),
        end: ts.getLineAndCharacterOfPosition(sourceFile, ts.textSpanEnd(rename.triggerSpan)),
      },
    };
  }

  rename(params: lsp.RenameParams): lsp.WorkspaceEdit | null {
    const fileName = params.textDocument.uri;
    const sourceFile = this.host.getSourceFile(fileName);
    if (sourceFile == null) {
      return null;
    }

    const position = ts.getPositionOfLineAndCharacter(
      sourceFile,
      params.position.line,
      params.position.character,
    );
    const rename = this.languageService.findRenameLocations(brioche.toTsUrl(fileName), position, false, false, {});
    if (rename == null) {
      return null;
    }

    const changes: Record<string, lsp.TextEdit[]> = {};
    for (const renameLocation of rename) {
      const renameSourceFile = this.host.getSourceFile(renameLocation.fileName);
      if (renameSourceFile == null) {
        continue;
      }

      const uri = brioche.fromTsUrl(renameLocation.fileName);
      const textEdits = changes[uri] ?? [];
      textEdits.push({
        range: {
          start: ts.getLineAndCharacterOfPosition(renameSourceFile, renameLocation.textSpan.start),
          end: ts.getLineAndCharacterOfPosition(renameSourceFile, ts.textSpanEnd(renameLocation.textSpan)),
        },
        newText: params.newName,
      });
      changes[uri] = textEdits;
    }

    return { changes };
  }
}
