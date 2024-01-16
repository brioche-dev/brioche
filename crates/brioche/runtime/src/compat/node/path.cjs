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
    return path.split("/").at(-1)?.split(".")?.at(-1) ?? "";
}

function join(...paths) {
    return paths.join("/");
}

function isAbsolute(path) {
    return path.startsWith("file://");
}

module.exports = {
    default: module.exports,
    posix: module.exports,
    dirname,
    extname,
    join,
    isAbsolute,
};
