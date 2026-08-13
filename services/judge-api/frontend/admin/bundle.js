const node = (tag, text) => {
  const element = document.createElement(tag);
  element.textContent = text;
  return element;
};

export async function activate(host) {
  return {
    async mount(_surfaceId, root) {
      const title = node("h1", host.i18n.translate("judge.admin.title"));
      const summary = node("p", host.i18n.translate("judge.admin.loading"));
      root.replaceChildren(title, summary);
      try {
        const result = await host.client.request("getJudgeQueue");
        const pending = Number(result?.pending ?? 0);
        summary.textContent = `${host.i18n.translate("judge.admin.pending")}: ${pending}`;
      } catch (error) {
        host.logger.error("load judge queue failed", {error: String(error)});
        summary.textContent = host.i18n.translate("judge.admin.failed");
      }
      return {dispose() { root.replaceChildren(); }};
    },
    async dispose() {},
  };
}
