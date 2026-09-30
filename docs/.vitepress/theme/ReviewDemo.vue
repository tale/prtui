<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import recording from "../../overview.mp4";
import poster from "../../overview.webp";

const video = ref<HTMLVideoElement | null>(null);
let preference: MediaQueryList | undefined;

function stopForReducedMotion() {
  if (!preference?.matches || !video.value) return;

  video.value.autoplay = false;
  video.value.pause();
}

onMounted(() => {
  preference = window.matchMedia("(prefers-reduced-motion: reduce)");
  preference.addEventListener("change", stopForReducedMotion);
  if (preference.matches || !video.value) return;

  video.value.muted = true;
  video.value.autoplay = true;
  void video.value.play().catch(() => {});
});

onUnmounted(() => {
  preference?.removeEventListener("change", stopForReducedMotion);
});
</script>

<template>
  <div class="review-demo">
    <video
      ref="video"
      :src="recording"
      :poster="poster"
      width="2304"
      height="1440"
      controls
      muted
      playsinline
      preload="none"
      aria-label="Watch a pull request review from selecting lines to submitting a comment"
    >
      <a :href="recording">Download the first review video.</a>
    </video>
  </div>
</template>
