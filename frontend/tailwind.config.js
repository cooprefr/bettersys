/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
  ],
  theme: {
    extend: {
      colors: {
        // BETTER Brand Palette (per brandbook)
        'void': '#000000',              // Dark-Hole
        'surface': '#0A0A0A',           // Neutral near-black surface for cards
        'better-blue': '#0026FF',       // Better Blue
        'better-blue-light': '#8A94C5', // UI-friendly light blue/steel
        // High-contrast metadata tint: near-white with a lavender infusion.
        'better-blue-lavender': '#E7E4FF',
        'grey': '#4C526F',              // Brey
        'dark-code': '#101010',      // Alternative dark
        // Semantic colors
        'success': '#00FF00',        // Pure green for BUY
        'danger': '#FF0000',         // Pure red for SELL
        'warning': '#FFAA00',        // Amber warning
        // Legacy mappings for gradual migration
        'lavender': {
          DEFAULT: '#DFDFDF',
          dim: '#808080',
        },
      },
      fontFamily: {
        'mono': ['"IBM Plex Mono"', 'monospace'],
      },
      animation: {
        'pulse-slow': 'pulse 3s ease-in-out infinite',
      },
    },
  },
  plugins: [],
}
