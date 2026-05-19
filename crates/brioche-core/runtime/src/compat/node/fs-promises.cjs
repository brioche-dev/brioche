// Stub for node:fs/promises
const unavailable = (name) => async () => {
    throw new Error(`fs/promises.${name} not available`);
};

module.exports.readFile = unavailable("readFile");
module.exports.writeFile = unavailable("writeFile");
module.exports.stat = unavailable("stat");
module.exports.access = unavailable("access");
