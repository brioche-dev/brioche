/**
 * This module implements a subset of the NodeJS `path` API, tailored for
 * supporting ESLint and its transitive dependencies. Because the Brioche
 * LSP uses URLs for all file paths, these functions all operate on
 * URL strings.
 */

// TODO: Figure out if this works with Windows paths / switch to
// proper URL parsing if needed

function dirname(path) {
    return path.split("/").slice(0, -1).join("/");
}

function extname(path) {
    const name = path.split("/").at(-1) ?? "";
    const dotIndex = name.lastIndexOf(".");
    return dotIndex > 0 ? name.slice(dotIndex) : "";
}

function join(...paths) {
    return paths.join("/");
}

function isAbsolute(path) {
    return path.startsWith("file://");
}

function resolve(...paths) {
    let result = paths[0] || "";
    for (let i = 1; i < paths.length; i++) {
        const segment = paths[i];
        if (isAbsolute(segment)) {
            result = segment;
        } else {
            result = result.replace(/\/$/, "") + "/" + segment;
        }
    }
    return result;
}

function basename(path, ext) {
    const base = path.split("/").at(-1) ?? "";
    if (ext && base.endsWith(ext)) {
        return base.slice(0, -ext.length);
    }
    return base;
}

function normalize(path) {
    const parts = path.split("/");
    const result = [];
    for (const part of parts) {
        if (part === "..") {
            result.pop();
        } else if (part !== ".") {
            result.push(part);
        }
    }
    return result.join("/");
}

function relative(from, to) {
    const fromParts = from.replace(/\/$/, "").split("/");
    const toParts = to.replace(/\/$/, "").split("/");
    let common = 0;
    while (common < fromParts.length && common < toParts.length && fromParts[common] === toParts[common]) {
        common++;
    }
    const ups = fromParts.length - common;
    const remainder = toParts.slice(common);
    const parts = [];
    for (let i = 0; i < ups; i++) {
        parts.push("..");
    }
    parts.push(...remainder);
    return parts.join("/");
}

function toNamespacedPath(path) {
    return path;
}

function parse(path) {
    const dir = dirname(path);
    const base = basename(path);
    const ext = extname(path);
    const name = ext ? base.slice(0, -ext.length) : base;
    return { root: "", dir, base, ext, name };
}

const pathExports = {
    sep: "/",
    delimiter: ":",
    dirname,
    extname,
    join,
    isAbsolute,
    resolve,
    basename,
    normalize,
    relative,
    toNamespacedPath,
    parse,
};

// Support both default and named exports
pathExports.default = pathExports;
pathExports.posix = pathExports;

module.exports = pathExports;
