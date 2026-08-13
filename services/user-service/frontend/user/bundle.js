const element = (tag, text) => {
  const node = document.createElement(tag);
  node.textContent = text;
  return node;
};

export async function activate(host) {
  return {
    async mount(_surfaceId, root) {
      const title = element("h1", host.i18n.translate("user.profile.title"));
      const summary = element("p", host.i18n.translate("user.profile.loading"));
      root.replaceChildren(title, summary);
      try {
        const profile = await host.client.request("getMyUserProfile");
        summary.textContent = profile?.display_name || profile?.user_id || "";
      } catch (error) {
        host.logger.error("load user profile failed", {error: String(error)});
        summary.textContent = host.i18n.translate("user.profile.failed");
      }
      return {dispose() { root.replaceChildren(); }};
    },
    async dispose() {},
  };
}
