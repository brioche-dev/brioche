import type * as eslint from "eslint";
import type * as estree from "estree";

function dirBasename(filename: string): string | undefined {
  return filename.split("/").at(-2);
}

function isNameKey(key: estree.Property["key"]): boolean {
  return (
    (key.type === "Identifier" && key.name === "name") ||
    (key.type === "Literal" && key.value === "name")
  );
}

function findProjectNameLiteral(
  exportNode: estree.ExportNamedDeclaration,
): estree.Literal | undefined {
  const { declaration } = exportNode;
  if (
    declaration?.type !== "VariableDeclaration" ||
    declaration.kind !== "const"
  ) {
    return undefined;
  }
  const [declarator] = declaration.declarations;
  if (
    declarator?.id.type !== "Identifier" ||
    declarator.id.name !== "project" ||
    declarator.init?.type !== "ObjectExpression"
  ) {
    return undefined;
  }
  const nameProp = declarator.init.properties.find(
    (prop): prop is estree.Property =>
      prop.type === "Property" && isNameKey(prop.key),
  );
  return nameProp?.value.type === "Literal" ? nameProp.value : undefined;
}

const rule: eslint.Rule.RuleModule = {
  meta: {
    type: "problem",
    docs: {
      description:
        "require project.name to match the project directory name",
    },
    messages: {
      mismatch:
        "Project name '{{actual}}' does not match the project directory name '{{expected}}'",
    },
  },
  create(context) {
    const { filename } = context;
    if (!filename.endsWith("/project.bri.ts")) {
      return {};
    }
    const expected = dirBasename(filename);
    if (expected == null || expected === "") {
      return {};
    }

    return {
      ExportNamedDeclaration(node: eslint.Rule.Node) {
        const nameLiteral = findProjectNameLiteral(
          node as unknown as estree.ExportNamedDeclaration,
        );
        if (nameLiteral == null || typeof nameLiteral.value !== "string") {
          return;
        }
        if (nameLiteral.value === expected) {
          return;
        }
        context.report({
          node: nameLiteral as unknown as eslint.Rule.Node,
          messageId: "mismatch",
          data: { actual: nameLiteral.value, expected },
        });
      },
    };
  },
};

export default rule;
