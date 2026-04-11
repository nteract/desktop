import { defineConfig } from "@argo-video/cli";

export default defineConfig({
  baseURL: "http://localhost:5173",
  demosDir: "demos",
  outputDir: "videos",
  tts: { defaultVoice: "af_heart" },
  video: {
    width: 1920,
    height: 1080,
    fps: 30,
    browser: "chromium",
  },
  export: {
    preset: "slow",
    crf: 18,
    audio: { loudnorm: true },
    sharpen: true,
  },
  overlays: {
    autoBackground: false,
  },
});
