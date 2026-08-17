/**
 * Status-bar state machine: loading / success / error presentation,
 * elapsed-time ticker, and progress counters.
 */

interface StatusState {
  loading: boolean;
  text: string;
  url: string;
  startTime: number | null;
  timerInterval: number | null;
  successCount: number;
  errorCount: number;
  current: number;
  total: number;
  errors: string[];
}

const statusState: StatusState = {
  loading: false,
  text: "Ready",
  url: "",
  startTime: null,
  timerInterval: null,
  successCount: 0,
  errorCount: 0,
  current: 0,
  total: 0,
  errors: [],
};

// 更新状态栏
function updateStatusBar() {
  const statusBar = document.getElementById("status-bar");
  const statusText = document.getElementById("status-text");
  const statusTimer = document.getElementById("status-timer");
  const statusProgress = document.getElementById("status-progress");
  const statusCount = document.getElementById("status-count");

  if (!statusBar || !statusText || !statusTimer || !statusProgress || !statusCount) return;

  statusBar.classList.remove("loading", "success", "error");

  if (statusState.loading) {
    statusBar.classList.add("loading");
    // 显示简短的标题，而不是完整URL
    if (statusState.text) {
      statusText.textContent = statusState.text.length > 30
        ? statusState.text.substring(0, 28) + "..."
        : statusState.text;
    } else {
      statusText.textContent = "Loading...";
    }
    if (statusState.startTime) {
      const elapsed = Math.floor((Date.now() - statusState.startTime) / 1000);
      statusTimer.textContent = `${elapsed}s`;
    } else {
      statusTimer.textContent = "";
    }
    if (statusState.total > 0) {
      statusProgress.textContent = `${statusState.current}/${statusState.total}`;
    } else {
      statusProgress.textContent = "";
    }
    // 简化显示：只显示总数和错误数
    const completed = statusState.successCount + statusState.errorCount;
    if (completed > 0) {
      if (statusState.errorCount > 0) {
        statusCount.textContent = `✗${statusState.errorCount}`;
        statusCount.classList.add("error");
      } else {
        statusCount.textContent = `✓${completed}`;
        statusCount.classList.remove("error");
      }
    } else {
      statusCount.textContent = "";
      statusCount.classList.remove("error");
    }
  } else if (statusState.errors.length > 0) {
    statusBar.classList.add("error");
    statusText.textContent = statusState.errors[0];
    statusTimer.textContent = "";
    statusProgress.textContent = "";
    statusCount.textContent = "";
    statusCount.classList.remove("error");
  } else {
    statusBar.classList.add("success");
    statusText.textContent = statusState.text || "Ready";
    statusTimer.textContent = "";
    statusProgress.textContent = "";
    statusCount.textContent = "";
    statusCount.classList.remove("error");
  }
}

// 设置加载状态
export function setLoadingWithStatus(url: string, text: string) {
  statusState.loading = true;
  statusState.url = url;
  statusState.text = text;
  statusState.startTime = Date.now();
  if (statusState.timerInterval) clearInterval(statusState.timerInterval);
  statusState.timerInterval = window.setInterval(() => updateStatusBar(), 1000);
  updateStatusBar();
}

// 清除加载状态
export function clearLoadingStatus(success: boolean, text: string) {
  statusState.loading = false;
  statusState.url = "";
  statusState.text = text;
  statusState.startTime = null;
  if (statusState.timerInterval) {
    clearInterval(statusState.timerInterval);
    statusState.timerInterval = null;
  }
  updateStatusBar();
  if (!success) {
    setTimeout(() => {
      statusState.errors = [];
      updateStatusBar();
    }, 5000);
  }
}

// Reset counts
export function resetCounts() {
  statusState.successCount = 0;
  statusState.errorCount = 0;
  statusState.current = 0;
  statusState.total = 0;
  statusState.errors = [];
  updateStatusBar();
}

export function incrementError(error: string) {
  statusState.errorCount++;
  statusState.errors.push(error);
  if (statusState.errors.length > 5) {
    statusState.errors.shift();
  }
  updateStatusBar();
}
