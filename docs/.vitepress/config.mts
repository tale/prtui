import { defineConfig, type HeadConfig } from "vitepress";

const siteUrl = "https://prtui.tale.me";

export default defineConfig({
  lang: "en-US",
  title: "prtui",
  description: "Review pull requests and local changes from the terminal.",
  transformHead({ page, pageData, title, description }) {
    const image = `${siteUrl}${pageData.frontmatter.ogImage ?? "/og/overview.jpg"}`;
    const imageAlt =
      pageData.frontmatter.ogImageAlt ?? "prtui displaying a pull request diff in the terminal";
    const properties = {
      "og:type": "website",
      "og:site_name": "prtui",
      "og:title": title,
      "og:description": description,
      "og:url": `${siteUrl}/${page === "index.html" ? "" : page}`,
      "og:image": image,
      "og:image:type": "image/jpeg",
      "og:image:width": "1200",
      "og:image:height": "630",
      "og:image:alt": imageAlt,
    };
    const twitter = {
      "twitter:card": "summary_large_image",
      "twitter:title": title,
      "twitter:description": description,
      "twitter:image": image,
      "twitter:image:alt": imageAlt,
    };

    return [
      ...Object.entries(properties).map(([property, content]): HeadConfig => [
        "meta",
        { property, content },
      ]),
      ...Object.entries(twitter).map(([name, content]): HeadConfig => ["meta", { name, content }]),
    ];
  },
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
