const element = (tag, text) => {
  const node = document.createElement(tag);
  node.textContent = text;
  return node;
};

export async function activate(host) {
  return {
    async mount(_surfaceId, root) {
      root.replaceChildren(
        element("h1", host.i18n.translate("user.admin.profiles.title")),
        element("p", host.i18n.translate("user.admin.profiles.select")),
      );
      return {dispose() { root.replaceChildren(); }};
    },
    async dispose() {},
  };
}
