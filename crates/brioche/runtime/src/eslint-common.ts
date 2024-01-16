import * as eslint from "eslint";
import * as typescriptEslintParser from "@typescript-eslint/parser";
import * as typescriptEslintPlugin from "@typescript-eslint/eslint-plugin";
import ts from "typescript";

export function buildLinter(): eslint.Linter {
  const linter = new eslint.Linter();

  linter.defineParser("@typescript-eslint/parser", typescriptEslintParser as eslint.Linter.ParserModule);

  for (let [name, rule] of Object.entries(typescriptEslintPlugin.rules)) {
    linter.defineRule(
      `@typescript-eslint/${name}`,
      rule as unknown as eslint.Rule.RuleModule,
    );
  }

  return linter;
}

export function buildEslintConfig(programs: ts.Program[]): eslint.Linter.Config {
  return {
    rules: {
      "@typescript-eslint/no-unused-vars": "warn",
    },
    parser: "@typescript-eslint/parser",
    parserOptions: {
      ecmaVersion: 2022,
      sourceType: "module",
      programs,
    },
  };
};
