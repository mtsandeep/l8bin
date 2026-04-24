import { defineConfig, mergeConfig } from "vite";
import config from "./vite.config.js";

export default mergeConfig(config, {
  server: {
    watch: {
      usePolling: true,
    },
    proxy: {
      "/api": {
        target: "http://api:3000",
        changeOrigin: true,
      },
    },
  },
});
