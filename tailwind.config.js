/** @type {import('tailwindcss').Config} */
module.exports = {
  content: ["./src/**/*.{ts,tsx}", "./apps/*/src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        uv: "#de5fe9",
      },
    },
  },
  plugins: [],
};
