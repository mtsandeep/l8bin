import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [
    react(),
    tailwindcss({
      theme: {
        extend: {
          colors: {
            'retro-bg': '#FDFBF7',
            'retro-ink': '#111827',
            'retro-accent': '#FF4B3E',
            'retro-blue': '#2563EB',
            'retro-surface': '#FFFFFF',
            'retro-gray': '#F3F4F6',
            'retro-text': '#374151',
            'retro-muted': '#9CA3AF',
            'retro-green': '#10B981',
            'retro-yellow': '#F59E0B',
          },
          fontFamily: {
            'mono': ['"Space Mono"', 'monospace'],
            'sans': ['"Work Sans"', 'sans-serif'],
          },
          boxShadow: {
            'neubrutalism': '4px 4px 0px 0px rgba(17, 24, 39, 1)',
            'neubrutalism-lg': '8px 8px 0px 0px rgba(17, 24, 39, 1)',
            'neubrutalism-xl': '12px 12px 0px 0px rgba(17, 24, 39, 1)',
            'neubrutalism-sm': '2px 2px 0px 0px rgba(17, 24, 39, 1)',
          },
        },
      },
    }),
  ],
});
