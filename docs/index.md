---
layout: home
markdownStyles: false
title: Review code from your terminal
description: Review pull requests and local changes with Vim motions.
---

<script setup>
import { withBase } from 'vitepress'
import ReviewDemo from './.vitepress/theme/ReviewDemo.vue'
</script>

<section class="home-review" aria-labelledby="home-title">
  <div class="home-intro">
    <h1 id="home-title">prtui</h1>
    <p>Review pull requests and local changes from your terminal.</p>
    <div class="home-actions">
      <a class="home-start" :href="withBase('/getting-started')">Get started</a>
      <a class="home-source" href="https://github.com/tale/prtui">Source <span aria-hidden="true">↗</span></a>
    </div>
  </div>
  <ReviewDemo />
</section>
