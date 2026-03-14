import "./index.css";
if (import.meta.env.DEV) {
  await import("../src/lib/connect-react-devtools");
}

const [{ StrictMode }, { createRoot }, { default: App }] = await Promise.all([
  import("react"),
  import("react-dom/client"),
  import("./App"),
]);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
