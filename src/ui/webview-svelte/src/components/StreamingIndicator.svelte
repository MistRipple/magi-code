<script lang="ts">
  // 流式输出时的打字指示器
</script>

<span class="streaming-indicator">
  <span class="dot"></span>
  <span class="dot"></span>
  <span class="dot"></span>
</span>

<style>
  .streaming-indicator {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    margin-left: var(--spacing-xs);
    vertical-align: middle;
  }

  .dot {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: var(--info);
    animation: bounce 1.4s ease-in-out infinite;
  }

  .dot:nth-child(1) {
    animation-delay: 0s;
  }

  .dot:nth-child(2) {
    animation-delay: 0.2s;
  }

  .dot:nth-child(3) {
    animation-delay: 0.4s;
  }

  @keyframes bounce {
    0%, 80%, 100% {
      transform: translateY(0);
      opacity: 0.4;
    }
    40% {
      transform: translateY(-4px);
      opacity: 1;
    }
  }
</style>

