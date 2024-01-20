const ops = (globalThis as any).Deno.core.ops;

function encodeUtf8(input: string): Uint8Array {
  return ops.op_brioche_utf8_encode(input);
}

function decodeUtf8(input: Uint8Array): string {
  return ops.op_brioche_utf8_decode(input);
}

const { TextEncoderClass, TextDecoderClass } = (() => {
  class TextEncoder {
    constructor() {}

    readonly encoding = "utf-8";

    encode(input?: string): Uint8Array {
      return encodeUtf8(input ?? "");
    }

    encodeInto(source: string, destination: Uint8Array): TextEncoderEncodeIntoResult {
      const buffer = encodeUtf8(source);

      // TODO: Fix this, this is wrong
      destination.set(buffer);

      // TODO: Fix this too
      return {
        read: source.length,
        written: Math.min(buffer.length, destination.length),
      };
    }
  }

  class TextDecoder {
    constructor() {}

    readonly encoding = "utf-8";

    readonly fatal = false;

    readonly ignoreBOM = false;

    decode(input?: ArrayBuffer | ArrayBufferView): string {
      if (input === undefined) {
        return "";
      } else if ("buffer" in input) {
        const buffer = input.buffer.slice(input.byteOffset, input.byteOffset + input.byteLength);
        return decodeUtf8(new Uint8Array(buffer));
      } else {
        return decodeUtf8(new Uint8Array(input));
      }
    }
  }

  return { TextEncoderClass: TextEncoder, TextDecoderClass: TextDecoder };
})();

export { TextEncoderClass as TextEncoder };
export { TextDecoderClass as TextDecoder };
