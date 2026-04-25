import "./console";
import { TextEncoder, TextDecoder } from "./text-encoding";

globalThis["TextEncoder"] = TextEncoder;
globalThis["TextDecoder"] = TextDecoder;

// structuredClone polyfill
if (typeof globalThis.structuredClone === "undefined") {
    globalThis.structuredClone = (value) => JSON.parse(JSON.stringify(value));
}

// URL polyfill
if (typeof globalThis.URL === "undefined") {
    globalThis.URL = class URL {
        constructor(input, base) {
            if (typeof input !== "string") {
                input = String(input);
            }
            if (base !== undefined) {
                if (typeof base !== "string") base = String(base);
                // Simple base resolution for relative URLs
                if (!/^\w+:/.test(input)) {
                    input = base.replace(/\/[^/]*$/, "/") + input;
                }
            }
            this.href = input;
            const protoMatch = input.match(/^(\w+):(\/\/)?/);
            this.protocol = protoMatch ? protoMatch[1] + ":" : "";
            const rest = protoMatch ? input.slice(protoMatch[0].length) : input;
            const qIndex = rest.indexOf("?");
            const hIndex = rest.indexOf("#");
            const pathEnd = qIndex !== -1 ? qIndex : hIndex !== -1 ? hIndex : rest.length;
            this.pathname = rest.slice(0, pathEnd) || "/";
            this.search = qIndex !== -1 ? rest.slice(qIndex, hIndex !== -1 ? hIndex : rest.length) : "";
            this.hash = hIndex !== -1 ? rest.slice(hIndex) : "";
            this.host = "";
            this.hostname = "";
            this.port = "";
            this.origin = this.protocol ? this.protocol + "//" : "";
        }
        toString() { return this.href; }
    };
}

globalThis["process"] = {
    env: {},
    cwd: () => "/",
    versions: { node: "0.0.0" },
    stderr: {},
    stdout: {},
    hrtime: Object.assign(
        (time) => {
            const now = Date.now();
            const sec = Math.floor(now / 1000);
            const nsec = (now % 1000) * 1e6;
            if (time) {
                const diffSec = sec - time[0];
                const diffNsec = nsec - time[1];
                return diffNsec < 0
                    ? [diffSec - 1, 1e9 + diffNsec]
                    : [diffSec, diffNsec];
            }
            return [sec, nsec];
        },
        {
            bigint: () => BigInt(Date.now()) * 1000000n,
        }
    ),
}
globalThis["require"] = {
    resolve: () => {},
}
