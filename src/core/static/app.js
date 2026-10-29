// Progressive enhancements: confirmations, field toggling, live relative times,
// reminder notifications and refreshing list pages after a long absence.
(() => {
  "use strict";

  document.addEventListener("submit", (event) => {
    const message = event.target.dataset.confirm;
    if (message && !window.confirm(message)) event.preventDefault();
  });

  // Show only the inputs that belong to the chosen reminder or repeat mode.
  for (const box of document.querySelectorAll("[data-switch]")) {
    const mode = box.querySelector("[data-switch-mode]");
    const sync = () => {
      for (const field of box.querySelectorAll("[data-show-for]")) {
        const visible = field.dataset.showFor.split(" ").includes(mode.value);
        field.hidden = !visible;
        field.disabled = !visible;
      }
    };
    mode.addEventListener("change", sync);
    sync();
  }

  // ---------------------------------------------------------------- live times

  // The server clock, so relative times match the server's even if this computer's is off.
  const clockOffset = Number(document.body.dataset.now || Date.now()) - Date.now();
  const now = () => Date.now() + clockOffset;

  // Same wording as src/core/src/time.rs.
  function amount(diff) {
    const minutes = Math.abs(Math.trunc(diff / 60000));
    if (minutes < 60) return `${Math.max(minutes, 1)} 分鐘`;
    if (minutes < 24 * 60) return `${Math.trunc(minutes / 60)} 小時`;
    return `${Math.trunc(minutes / (24 * 60))} 天`;
  }

  function refreshTimes() {
    const t = now();
    for (const el of document.querySelectorAll("[data-rel]")) {
      const at = Number(el.dataset.rel);
      el.textContent = amount(at - t) + (at >= t ? "後" : "前");
    }
    for (const el of document.querySelectorAll("[data-due]")) {
      const due = Number(el.dataset.due);
      el.textContent = due >= t ? `${amount(due - t)}後到期` : `已逾期 ${amount(due - t)}`;
      el.classList.toggle("overdue", due < t);
      el.classList.toggle("soon", due >= t && due - t < 24 * 3600000);
    }
    for (const el of document.querySelectorAll("[data-until]")) {
      el.hidden = t >= Number(el.dataset.until);
    }
    for (const el of document.querySelectorAll("[data-from]")) {
      el.hidden = t < Number(el.dataset.from);
    }
  }

  refreshTimes();
  setInterval(refreshTimes, 30_000);

  if (!document.body.dataset.poll) return;

  // ---------------------------------------------------------------- notification permission

  const canNotify = "Notification" in window;
  const toasts = document.getElementById("toasts");

  function showToast(title, detail, url) {
    const toast = document.createElement("div");
    toast.className = "toast";
    toast.setAttribute("role", "alert");
    const heading = document.createElement(url ? "a" : "strong");
    if (url) heading.href = url;
    heading.textContent = title;
    const text = document.createElement("p");
    text.textContent = detail;
    const close = document.createElement("button");
    close.className = "toast-close";
    close.type = "button";
    close.setAttribute("aria-label", "關閉");
    close.textContent = "×";
    close.addEventListener("click", () => toast.remove());
    toast.append(heading, text, close);
    toasts.append(toast);
  }

  /** Returns false when the browser cannot show it (no permission or no support). */
  function showSystemNotification(title, body, url, tag) {
    if (!canNotify || Notification.permission !== "granted") return false;
    try {
      const notification = new Notification(title, { body, tag, requireInteraction: true });
      notification.addEventListener("click", () => {
        window.focus();
        window.location.href = url;
        notification.close();
      });
      return true;
    } catch {
      // Android Chrome only allows notifications from a service worker.
      return false;
    }
  }

  const BLOCKED_HELP =
    "請點網址列左側的網站設定，把「通知」改為允許後重新整理；macOS 另需在「系統設定 › 通知」開啟瀏覽器的通知。";

  const permissionLabels = {
    granted: "已允許",
    denied: "已封鎖",
    default: "尚未設定",
  };

  const notifyButton = document.getElementById("notify-btn");
  const notifyStatus = document.getElementById("notify-status");

  function syncPermission() {
    const state = canNotify ? Notification.permission : "unsupported";
    if (notifyStatus) {
      notifyStatus.textContent = permissionLabels[state] || "這個瀏覽器不支援系統通知，只會在頁面上提示";
    }
    if (!notifyButton) return;
    notifyButton.hidden = state === "granted" || state === "unsupported";
    if (state === "denied") {
      notifyButton.textContent = "🔕 通知已封鎖";
      notifyButton.title = "瀏覽器封鎖了這個網站的通知，點一下查看如何開啟";
    } else {
      notifyButton.textContent = "🔔 啟用通知";
      notifyButton.title = "提醒時間到時以系統通知提示";
    }
  }

  async function askPermission() {
    if (canNotify && Notification.permission === "default") {
      await Notification.requestPermission();
    }
    syncPermission();
    return canNotify ? Notification.permission : "unsupported";
  }

  notifyButton?.addEventListener("click", async () => {
    if ((await askPermission()) === "denied") showToast("🔕 通知已封鎖", BLOCKED_HELP);
  });

  document.getElementById("notify-test")?.addEventListener("click", async () => {
    const state = await askPermission();
    if (state === "denied") {
      showToast("🔕 通知已封鎖", BLOCKED_HELP);
      return;
    }
    const shown = showSystemNotification(
      "測試通知",
      "看到這則通知，代表提醒可以正常跳出。",
      "/password",
      "test",
    );
    showToast(
      "已送出測試通知",
      shown
        ? "如果沒有看到系統通知，請檢查作業系統的通知設定與專注模式／勿擾模式。"
        : "這個瀏覽器無法顯示系統通知，提醒只會顯示在頁面上。",
    );
  });

  // Reflect changes made in the browser's site settings without a reload.
  navigator.permissions
    ?.query({ name: "notifications" })
    .then((status) => status.addEventListener("change", syncPermission))
    .catch(() => {});
  syncPermission();

  // ---------------------------------------------------------------- reminders

  const baseTitle = document.title;
  let unseen = 0;

  function announce(reminder) {
    showToast("⏰ " + reminder.title, reminder.detail, reminder.url);
    showSystemNotification("工作提醒：" + reminder.title, reminder.detail, reminder.url, "reminder-" + reminder.issue_id);
    for (const item of document.querySelectorAll(`[data-reminder-item="${reminder.issue_id}"]`)) {
      item.remove();
    }
    if (document.hidden) {
      unseen += 1;
      document.title = `(${unseen}) ⏰ ${baseTitle}`;
    }
  }

  // Poll when the next reminder is due (the server says when), and at least every minute
  // to pick up reminders set in other tabs or by edits.
  const MAX_WAIT = 60_000;
  let timer = 0;
  let polling = false;

  function schedule(delay) {
    clearTimeout(timer);
    timer = setTimeout(poll, Math.min(Math.max(delay, 1000), MAX_WAIT));
  }

  async function poll() {
    if (polling) return;
    polling = true;
    let delay = MAX_WAIT;
    try {
      const response = await fetch("/api/reminders", {
        credentials: "same-origin",
        headers: { Accept: "application/json" },
      });
      if (!response.ok) return;
      const { reminders, next_in_ms: nextIn } = await response.json();
      if (typeof nextIn === "number") delay = nextIn + 500;
      for (const reminder of reminders) {
        // Acknowledge first so other open tabs do not show it again.
        const ack = await fetch(`/api/reminders/${reminder.issue_id}/ack`, {
          method: "POST",
          credentials: "same-origin",
        });
        if (ack.ok) announce(reminder);
      }
    } catch {
      // Offline or server restarting; the next poll retries.
    } finally {
      polling = false;
      schedule(delay);
    }
  }

  poll();

  // ---------------------------------------------------------------- coming back to the tab

  // Lists and the dashboard reload after a long absence to show other people's changes.
  // Forms are never reloaded, and neither is a page still showing reminders.
  const RELOAD_AFTER = 5 * 60_000;
  const reloadable = /^\/(issues|board|team)?$/.test(location.pathname);
  let hiddenAt = document.hidden ? Date.now() : 0;

  document.addEventListener("visibilitychange", () => {
    if (document.hidden) {
      hiddenAt = Date.now();
      return;
    }
    unseen = 0;
    document.title = baseTitle;
    const away = hiddenAt ? Date.now() - hiddenAt : 0;
    hiddenAt = 0;
    if (reloadable && away >= RELOAD_AFTER && !toasts.children.length) {
      location.reload();
      return;
    }
    refreshTimes();
    poll();
  });
})();
