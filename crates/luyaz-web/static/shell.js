// ⌘K / Ctrl+K page search (the list is in the page; this only filters and moves the selection),
// and keeps the old web's language in step with ours while both exist.
(function () {
  try { localStorage.setItem("u60_lang", document.documentElement.dataset.lang); } catch (e) {}
  var d = document.getElementById("palette");
  if (!d) return;
  var q = document.getElementById("palette-q");
  var items = Array.prototype.slice.call(d.querySelectorAll("a[data-k]"));
  var empty = d.querySelector(".t-palette__empty");
  var active = 0;
  function shown() { return items.filter(function (a) { return !a.parentNode.hidden; }); }
  function mark() {
    var s = shown();
    items.forEach(function (a) { a.removeAttribute("aria-selected"); });
    if (s[active]) { s[active].setAttribute("aria-selected", "true"); s[active].scrollIntoView({ block: "nearest" }); }
    empty.hidden = s.length > 0;
  }
  function filter() {
    var f = q.value.toLowerCase().replace(/\s+/g, "");
    items.forEach(function (a) { a.parentNode.hidden = f !== "" && a.dataset.k.indexOf(f) < 0; });
    active = 0;
    mark();
  }
  function open() { if (!d.open) { q.value = ""; filter(); d.showModal(); q.focus(); } }
  document.addEventListener("keydown", function (e) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") { e.preventDefault(); d.open ? d.close() : open(); }
  });
  document.querySelectorAll("[data-palette-open]").forEach(function (b) { b.addEventListener("click", open); });
  q.addEventListener("input", filter);
  q.addEventListener("keydown", function (e) {
    var s = shown();
    if (e.key === "ArrowDown") { e.preventDefault(); active = Math.min(active + 1, s.length - 1); mark(); }
    else if (e.key === "ArrowUp") { e.preventDefault(); active = Math.max(active - 1, 0); mark(); }
    else if (e.key === "Enter" && s[active]) { e.preventDefault(); location.href = s[active].href; }
  });
  d.addEventListener("click", function (e) { if (e.target === d) d.close(); });
})();
