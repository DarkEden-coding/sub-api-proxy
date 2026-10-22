import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Development only: point the proxy at a running sub-api-proxy with SAP_BACKEND=http://127.0.0.1:8317.
const backend = process.env.SAP_BACKEND || "http://127.0.0.1:8317";
const repoRoot = fileURLToPath(new URL("../..", import.meta.url));

export default defineConfig({
  base: "/sap/",
  plugins: [svelte({ compilerOptions: { discloseVersion: false } })],
  // Demo mode imports the sap-core fixture from outside this directory.
  server: { port: 5174, fs: { allow: [repoRoot] }, proxy: { "/sap/api": backend } },
  preview: { port: 4174 },
  build: { target: "es2022", sourcemap: false },
});
