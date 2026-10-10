// Connect page: fill in this server's address, copy buttons and client tabs.
(() => {
  const origin = location.origin;
  const token = "<token>";
  const fill = () => {
    document.querySelectorAll("[data-tpl]").forEach((n) => {
      if (!n.dataset.raw) n.dataset.raw = n.textContent;
      n.textContent = n.dataset.raw.replaceAll("{ORIGIN}", origin).replaceAll("{MCP}", `${origin}/mcp/${token}`).replaceAll("{TOKEN}", token);
    });
  };
  fill();

  document.querySelectorAll(".code").forEach((box) => {
    const b = document.createElement("button");
    b.className = "copy"; b.type = "button"; b.textContent = "Copy";
    b.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(box.querySelector("pre").textContent);
        b.textContent = "Copied";
        if (typeof window.gtag === "function") window.gtag("event", "copy_connect_snippet", { snippet: box.dataset.panel || (box.classList.contains("join") ? "join" : "other") });
      } catch (_) {
        b.textContent = "Select and copy";
      }
      setTimeout(() => (b.textContent = "Copy"), 1600);
    });
    box.append(b);
  });

  document.querySelectorAll(".tab").forEach((t) => t.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((x) => { x.setAttribute("aria-selected", String(x === t)); x.setAttribute("aria-pressed", String(x === t)); });
    document.querySelectorAll("[data-panel]").forEach((p) => (p.hidden = p.dataset.panel !== t.dataset.tab));
  }));

})();
