/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
  ],
  theme: {
    extend: {
      colors: {
        'void': 'rgb(var(--c-bg) / <alpha-value>)',
        'surface': 'rgb(var(--c-surface) / <alpha-value>)',
        'fg': 'rgb(var(--c-fg) / <alpha-value>)',
        'better-blue': 'rgb(var(--c-accent) / <alpha-value>)',
        'better-blue-light': '#8A94C5',
        'better-blue-lavender': '#E7E4FF',
        'grey': 'rgb(var(--c-grey) / <alpha-value>)',
        'dark-code': '#101010',
        'success': 'rgb(var(--c-success) / <alpha-value>)',
        'danger': 'rgb(var(--c-danger) / <alpha-value>)',
        'warning': 'rgb(var(--c-warning) / <alpha-value>)',
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
