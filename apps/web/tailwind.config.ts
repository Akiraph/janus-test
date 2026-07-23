import type { Config } from "tailwindcss";
import tailwindcssAnimate from "tailwindcss-animate";

export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      // Note: color / radius / font tokens live in styles.css (@theme inline)
      // and resolve from :root runtime variables so the whole layer is
      // theme-overridable. This config only holds non-token Tailwind extras.
      fontSize: {
        "2xs": ["0.6875rem", { lineHeight: "1rem" }], // 11px
        xs: ["0.75rem", { lineHeight: "1.05rem" }], // 12px
        sm: ["0.8125rem", { lineHeight: "1.25rem" }], // 13px — dense UI base
        base: ["0.875rem", { lineHeight: "1.4rem" }], // 14px
      },
      boxShadow: {
        card: "0 1px 2px hsl(210 30% 20% / 0.04), 0 1px 3px hsl(210 30% 20% / 0.06)",
        raised:
          "0 4px 12px hsl(210 30% 20% / 0.08), 0 2px 4px hsl(210 30% 20% / 0.04)",
        pop: "0 8px 28px hsl(210 30% 20% / 0.12)",
      },
      // Collapsible keyframes are Radix-driven (use --radix-collapsible-content-
      // height) and live here so the tailwindcss-animate plugin + Radix data
      // attributes can drive them. fade-in-up / route-fade / panel-in / live-
      // pulse are defined once in styles.css — no duplicate definitions.
      keyframes: {
        "collapsible-down": {
          from: { height: "0", opacity: "0" },
          to: {
            height: "var(--radix-collapsible-content-height)",
            opacity: "1",
          },
        },
        "collapsible-up": {
          from: {
            height: "var(--radix-collapsible-content-height)",
            opacity: "1",
          },
          to: { height: "0", opacity: "0" },
        },
      },
      animation: {
        "collapsible-down": "collapsible-down 0.2s ease-out",
        "collapsible-up": "collapsible-up 0.2s ease-out",
      },
    },
  },
  plugins: [tailwindcssAnimate],
} satisfies Config;
