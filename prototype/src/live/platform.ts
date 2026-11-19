export const isMacOS = () => /Mac/i.test(globalThis.navigator?.platform ?? "");
