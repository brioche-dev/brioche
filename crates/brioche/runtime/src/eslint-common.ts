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
      "eqeqeq": ["warn", "always", { null: "ignore" }],
      "no-console": ["warn", { allow: ["debug", "info", "warn", "error"] }],
      "no-debugger": "error",
      "no-delete-var": "warn",
      "no-invalid-regexp": "warn",
      "no-misleading-character-class": "warn",
      "no-new-symbol": "warn",
      "no-obj-calls": "warn",
      "no-octal": "warn",
      "no-octal-escape": "warn",
      "no-self-assign": "warn",
      "no-self-compare": "warn",
      "no-unreachable": "warn",
      "no-unreachable-loop": "warn",
      "no-unused-labels": "warn",
      "no-with": "warn",
      "use-isnan": "warn",
      "@typescript-eslint/adjacent-overload-signatures": "warn",
      "@typescript-eslint/array-type": "warn",
      "@typescript-eslint/await-thenable": "warn",
      "@typescript-eslint/ban-types": "warn",
      "@typescript-eslint/consistent-type-assertions": ["warn", {
        assertionStyle: "as",
        objectLiteralTypeAssertions: "allow-as-parameter",
      }],
      "@typescript-eslint/no-array-constructor": "warn",
      "@typescript-eslint/no-array-delete": "warn",
      "@typescript-eslint/no-confusing-non-null-assertion": "warn",
      "@typescript-eslint/no-confusing-void-expression": "warn",
      "@typescript-eslint/no-duplicate-enum-values": "error",
      "@typescript-eslint/no-duplicate-type-constituents": "warn",
      "@typescript-eslint/no-extra-non-null-assertion": "warn",
      "@typescript-eslint/no-floating-promises": "warn",
      "@typescript-eslint/no-extraneous-class": "warn",
      "@typescript-eslint/no-for-in-array": "warn",
      "@typescript-eslint/no-implied-eval": "warn",
      "@typescript-eslint/no-invalid-void-type": "warn",
      "@typescript-eslint/no-loss-of-precision": "warn",
      "@typescript-eslint/no-meaningless-void-operator": "warn",
      "@typescript-eslint/no-misused-new": "warn",
      "@typescript-eslint/no-misused-promises": "warn",
      "@typescript-eslint/no-mixed-enums": "warn",
      "@typescript-eslint/no-namespace": "warn",
      "@typescript-eslint/no-non-null-asserted-nullish-coalescing": "warn",
      "@typescript-eslint/no-non-null-asserted-optional-chain": "warn",
      "@typescript-eslint/no-non-null-assertion": "warn",
      "@typescript-eslint/no-redundant-type-constituents": "warn",
      "@typescript-eslint/no-throw-literal": ["warn", {
        allowThrowingAny: true,
        allowThrowingUnknown: true,
      }],
      "@typescript-eslint/no-unnecessary-boolean-literal-compare": "warn",
      "@typescript-eslint/no-unnecessary-condition": "warn",
      "@typescript-eslint/no-unnecessary-type-assertion": "warn",
      "@typescript-eslint/no-unnecessary-type-constraint": "warn",
      "@typescript-eslint/no-unsafe-declaration-merging": "warn",
      "@typescript-eslint/no-unsafe-enum-comparison": "warn",
      "@typescript-eslint/no-unused-vars": "warn",
      "@typescript-eslint/no-useless-constructor": "warn",
      "@typescript-eslint/prefer-for-of": "warn",
      "@typescript-eslint/prefer-includes": "warn",
      "@typescript-eslint/prefer-nullish-coalescing": "warn",
      "@typescript-eslint/prefer-optional-chain": "warn",
      "@typescript-eslint/prefer-promise-reject-errors": "warn",
      "@typescript-eslint/prefer-string-starts-ends-with": "warn",
      "@typescript-eslint/restrict-plus-operands": "warn",
      "@typescript-eslint/restrict-template-expressions": "warn",
      "@typescript-eslint/strict-boolean-expressions": ["warn", {
        allowString: false,
        allowNumber: false,
        allowNullableObject: false,
        allowNullableBoolean: false,
        allowNullableString: false,
        allowNullableNumber: false,
        allowNullableEnum: false,
        allowAny: false,
      }],
      "@typescript-eslint/triple-slash-reference": "error",
    },
    parser: "@typescript-eslint/parser",
    parserOptions: {
      ecmaVersion: 2022,
      sourceType: "module",
      programs,
    },
  };
};
