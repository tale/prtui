<script setup lang="ts">
import { computed, ref } from "vue";
import { data as schemes } from "./schemes.data";

const query = ref("");
const variant = ref<"all" | "dark" | "light">("all");
const copied = ref("");

type Token = [text: string, color?: string, background?: string];

// Laid out like a diff row: old and new line numbers, marker, sigil, code.
const preview: { background?: string; isHunk?: boolean; tokens: Token[] }[] = [
  { background: "hunk", isHunk: true, tokens: [["@@ -12,3 +12,4 @@", "muted"]] },
  {
    tokens: [
      ["  12   12   ", "comment"],
      ["fn", "magenta"],
      [" "],
      ["greet", "blue"],
      ["("],
      ["name", "red"],
      [": &"],
      ["str", "yellow"],
      [") {"],
    ],
  },
  {
    background: "delete",
    tokens: [
      ["  13       -", "comment"],
      ["    "],
      ["println!", "cyan"],
      ["("],
      ['"', "green"],
      ["Hi", "green", "deleteEmphasis"],
      [" ", "green"],
      ["{name}", "orange"],
      ['"', "green"],
      [");"],
    ],
  },
  {
    background: "add",
    tokens: [["       13  +", "comment"], ["    "], ["// Greets politely", "comment"]],
  },
  {
    background: "add",
    tokens: [
      ["       14  +", "comment"],
      ["    "],
      ["println!", "cyan"],
      ["("],
      ['"', "green"],
      ["Hello,", "green", "addEmphasis"],
      [" ", "green"],
      ["{name}", "orange"],
      ['"', "green"],
      [");"],
    ],
  },
  { tokens: [["  14   15   ", "comment"], ["}"]] },
];

const shown = computed(() => {
  const needle = query.value.trim().toLowerCase();
  return schemes.filter(
    (scheme) =>
      (variant.value === "all" || scheme.variant === variant.value) &&
      (scheme.slug.includes(needle) || scheme.name.toLowerCase().includes(needle)),
  );
});

async function copy(slug: string) {
  await navigator.clipboard.writeText(`--theme ${slug}`);
  copied.value = slug;
}
</script>

<template>
  <div class="scheme-controls">
    <input v-model="query" type="search" placeholder="Search schemes" aria-label="Search schemes" />
    <div class="scheme-variants" role="group" aria-label="Background">
      <button
        v-for="option in ['all', 'dark', 'light'] as const"
        :key="option"
        type="button"
        :aria-pressed="variant === option"
        @click="variant = option"
      >
        {{ option }}
      </button>
    </div>
    <span class="scheme-count">{{ shown.length }} schemes</span>
  </div>

  <ul class="scheme-grid">
    <li v-for="{ slug, name, author, colors: c } in shown" :key="slug">
      <div
        class="scheme-preview"
        :style="{ background: c.background, color: c.foreground }"
        aria-hidden="true"
      >
        <div
          v-for="(row, index) in preview"
          :key="index"
          :class="{ hunk: row.isHunk }"
          :style="{ background: row.background && c[row.background] }"
        >
          <span
            v-for="([text, color, background], token) in row.tokens"
            :key="token"
            :style="{ color: color && c[color], background: background && c[background] }"
            >{{ text }}</span
          >
        </div>
      </div>
      <div class="scheme-meta">
        <button type="button" :title="`Copy --theme ${slug}`" @click="copy(slug)">
          <code>{{ slug }}</code>
          <span class="scheme-copied" aria-live="polite">{{
            copied === slug ? "copied" : ""
          }}</span>
        </button>
        <span class="scheme-name"
          >{{ name }}<template v-if="author"> · {{ author }}</template></span
        >
      </div>
    </li>
  </ul>
</template>

<style scoped>
.scheme-controls {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;
  margin: 16px 0;
}

.scheme-controls input {
  flex: 1 1 200px;
  padding: 6px 10px;
  border: 1px solid var(--vp-c-divider);
  border-radius: 6px;
  background: var(--vp-c-bg-soft);
  font-size: 14px;
}

.scheme-variants {
  display: flex;
  border: 1px solid var(--vp-c-divider);
  border-radius: 6px;
  overflow: hidden;
}

.scheme-variants button {
  padding: 6px 12px;
  font-size: 13px;
  text-transform: capitalize;
  color: var(--vp-c-text-2);
}

.scheme-variants button[aria-pressed="true"] {
  background: var(--vp-c-brand-soft);
  color: var(--vp-c-brand-1);
}

.scheme-count {
  color: var(--vp-c-text-3);
  font-size: 13px;
}

.scheme-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
  gap: 16px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.scheme-grid li {
  margin: 0;
  content-visibility: auto;
  contain-intrinsic-size: auto 200px;
}

.scheme-preview {
  margin: 0;
  padding: 8px 0;
  overflow: hidden;
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
  font-family: var(--vp-font-family-mono);
  font-size: 12px;
  line-height: 1.6;
  white-space: pre;
}

.scheme-preview > div {
  padding: 0 8px;
}

.scheme-preview .hunk {
  font-style: italic;
}

.scheme-meta {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 8px 2px 0;
}

.scheme-meta button {
  display: flex;
  align-items: baseline;
  gap: 8px;
  text-align: left;
}

.scheme-meta button:hover code {
  color: var(--vp-c-brand-1);
}

.scheme-copied {
  color: var(--vp-c-text-3);
  font-size: 12px;
}

.scheme-name {
  overflow: hidden;
  color: var(--vp-c-text-2);
  font-size: 13px;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
