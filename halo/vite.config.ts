import { reactRouter } from "@react-router/dev/vite";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [tailwindcss(), reactRouter()],
  publicDir: false,
  ssr: { noExternal: ["ajv", "ajv-formats"] },
  base: `/assets/${process.env.HALO_BUILD_ID ?? "development"}/`,
  build: { assetsDir: "", sourcemap: false, assetsInlineLimit: 0 },
  define: {
    __BUILD_ID__: JSON.stringify(process.env.HALO_BUILD_ID ?? "development"),
  },
});
