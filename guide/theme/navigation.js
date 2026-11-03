// Keep Markdown readable on its own; add a local contents list to HTML pages.
(() => {
  if (window.location.pathname.endsWith("/print.html")) return;
  const main = document.querySelector(".content main");
  if (!main || main.querySelector(".page-toc")) return;

  const headings = Array.from(main.querySelectorAll("h2[id]"));
  const title = main.querySelector("h1");
  if (!title || headings.length < 3) return;

  const contents = document.createElement("details");
  contents.className = "page-toc";
  const summary = document.createElement("summary");
  summary.textContent = "On this page";
  contents.append(summary);

  const navigation = document.createElement("nav");
  navigation.setAttribute("aria-label", "On this page");
  const list = document.createElement("ol");
  for (const heading of headings) {
    const item = document.createElement("li");
    const link = document.createElement("a");
    link.href = `#${heading.id}`;
    link.textContent = heading.textContent;
    item.append(link);
    list.append(item);
  }
  navigation.append(list);
  contents.append(navigation);

  // Leave the chapter's opening sentence ahead of navigation when present.
  const introduction = title.nextElementSibling;
  const anchor = introduction?.tagName === "P" ? introduction : title;
  anchor.after(contents);
})();
