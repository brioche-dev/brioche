// Stub for node:module
module.exports.createRequire = () => ({
    resolve: () => {
        throw new Error("module.createRequire().resolve not available");
    },
});
