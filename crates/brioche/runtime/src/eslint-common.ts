import { Linter } from "eslint";
import * as typescriptEslintParser from "@typescript-eslint/parser";

export function buildLinter(): Linter {
  const linter = new Linter();

  linter.defineParser("@typescript-eslint/parser", typescriptEslintParser as Linter.ParserModule);

  return linter;
}

export const ESLINT_CONFIG: Linter.Config = {
  rules: {
    "no-unused-vars": "warn",
  },
  parser: "@typescript-eslint/parser",
  parserOptions: {
    ecmaVersion: 2022,
    sourceType: "module",
  },
};
