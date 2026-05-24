// Stub for node:url
module.exports.pathToFileURL = (path) => `file://${path}`;
module.exports.fileURLToPath = (url) => {
    const str = typeof url === "string" ? url : url.href;
    return str.startsWith("file://") ? str.slice("file://".length) : str;
};
