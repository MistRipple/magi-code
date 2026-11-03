export default async function activate(api) {
  await api.tool('note-count', async () => {
    const notes = await api.resource.read('notes');
    const value = notes.value;
    return { count: Array.isArray(value?.items) ? value.items.length : 0 };
  });
}
