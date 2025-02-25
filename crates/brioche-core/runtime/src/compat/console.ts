const ops = (globalThis as any).Deno.core.ops;

function logLevel(level: string, ...args: unknown[]) {
  ops.op_brioche_console(level, displayAll(...args).join(" "));
}

export function displayAll(...values: unknown[]): string[] {
  if (values.length === 0) {
    return [];
  }

  const displayed = [];
  const remaining = [...values];
  if (typeof remaining[0] === "string") {
    displayed.push(remaining.shift() as string);
  }

  for (const value of remaining) {
    displayed.push(display(value));
  }

  return displayed;
}

export function display(value: unknown): string {
  switch (typeof value) {
    case "string":
      return JSON.stringify(value);
    case "number":
    case "bigint":
    case "boolean":
    case "symbol":
      return value.toString();
    case "undefined":
      return "undefined";
    case "function":
      return "[function]";
    case "object":
      if (value === null) {
        return "null";
      } else if (value instanceof Error) {
        return value.stack ?? value.message;
      } else if (value instanceof RegExp) {
        return value.toString();
      } else if (value instanceof Date) {
        return value.toISOString();
      } else if (Array.isArray(value)) {
        const items = value.map(display);
        return `[${items.join(", ")}]`;
      } else {
        const entries = Object.entries(value).map(
          ([key, value]) => `${JSON.stringify(key)}: ${display(value)}`,
        );
        return `{${entries.join(", ")}}`;
      }
  }
}

(globalThis as any).console ??= {};
globalThis.console.log = (...args: unknown[]) => {
  logLevel("log", ...args);
};
globalThis.console.debug = (...args: unknown[]) => {
  logLevel("debug", ...args);
};
globalThis.console.info = (...args: unknown[]) => {
  logLevel("info", ...args);
};
globalThis.console.warn = (...args: unknown[]) => {
  logLevel("warn", ...args);
};
globalThis.console.error = (...args: unknown[]) => {
  logLevel("error", ...args);
};
