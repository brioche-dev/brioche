import "./console";
import { TextEncoder, TextDecoder } from "./text-encoding";

globalThis["TextEncoder"] = TextEncoder;
globalThis["TextDecoder"] = TextDecoder;

globalThis["process"] = {
    env: {},
    cwd: () => "",
    versions: { node: "0.0.0" },
    stderr: {},
    stdout: {},
}
globalThis["require"] = {
    resolve: () => {},
}
