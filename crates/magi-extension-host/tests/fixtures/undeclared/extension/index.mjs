export default async function activate(api) {
  await api.on('usage.update', () => {});
}
