import { defineConfig } from "tsup";

export default defineConfig({
  entry: ["src/index.ts"],
  format: ["esm"],
  // tsup 8 supplies baseUrl internally; TypeScript 6 still supports it.
  // Scope the compatibility setting to declaration generation.
  dts: { compilerOptions: { ignoreDeprecations: "6.0" } },
  sourcemap: true,
  clean: true,
  minify: false,
  target: "node18",
  shims: false,
});
