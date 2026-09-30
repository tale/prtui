import { defineConfig } from "vitepress";

export default defineConfig({
  lang: "en-US",
  title: "prtui",
  description: "Review pull requests and local changes from the terminal.",
  themeConfig: {
    nav: [
      { text: "Guide", link: "/getting-started" },
      { text: "Keys", link: "/keys" },
      { text: "Releases", link: "https://github.com/tale/prtui/releases" },
    ],
    sidebar: [
      { text: "Get started", link: "/getting-started" },
      { text: "Review a pull request", link: "/reviewing" },
      { text: "Review local changes", link: "/local-diffs" },
      { text: "Keyboard reference", link: "/keys" },
      { text: "CLI & configuration", link: "/cli" },
    ],
    search: { provider: "local" },
    outline: [2, 3],
    socialLinks: [
      { icon: "github", link: "https://github.com/tale/prtui" },
      { icon: "githubsponsors", link: "https://github.com/sponsors/tale" },
      { icon: "kofi", link: "https://ko-fi.com/atale" },
    ],
    editLink: {
      pattern: "https://github.com/tale/prtui/edit/main/docs/:path",
      text: "Edit this page",
    },
  },
});
