import nextCoreWebVitals from "eslint-config-next/core-web-vitals";
import nextTypeScript from "eslint-config-next/typescript";

const eslintConfig = [
  ...nextCoreWebVitals,
  ...nextTypeScript,
  {
    // eslint-config-next requests `settings.react.version: "detect"`. Detection calls the
    // `context.getFilename()` API that ESLint 10 removed, which crashes eslint-plugin-react@7.37.5
    // (its peer range still stops at eslint ^9.7). Pinning an explicit React version skips
    // detection and the removed API entirely. Matches the installed `react` (^19.2).
    settings: {
      react: {
        version: "19.2",
      },
    },
  },
  {
    // Root tooling configs (tailwind.config.js, postcss.config.js) are CommonJS modules; the
    // `require()` they use is correct there, so parse them as CommonJS and don't flag it.
    files: ["**/*.config.js"],
    languageOptions: {
      sourceType: "commonjs",
    },
    rules: {
      "@typescript-eslint/no-require-imports": "off",
    },
  },
  {
    // eslint-config-next 16 bundles eslint-plugin-react-hooks 7, whose `recommended` preset adds
    // the new React Compiler rules below. They were absent from the react-hooks 5 baseline this
    // repo linted against before the eslint 9 -> 10 bump, and they flag existing, working code.
    // Keep the pre-upgrade baseline (rules-of-hooks + exhaustive-deps stay on) and adopt these
    // opinionated rules in a dedicated follow-up rather than refactoring app state in a deps bump.
    rules: {
      "react-hooks/set-state-in-effect": "off",
      "react-hooks/set-state-in-render": "off",
      "react-hooks/purity": "off",
    },
  },
];

export default eslintConfig;
