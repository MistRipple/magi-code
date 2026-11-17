export function humanizeIdentifier(value: string): string {
  return value
    .trim()
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .replace(/[._-]+/g, ' ')
    .replace(/\s+/g, ' ')
    .toLowerCase();
}

export function getBuiltinToolFallbackLabel(name: string): string {
  return humanizeIdentifier(name) || 'unnamed built-in tool';
}

export function getCapabilityDependencyFallbackLabel(name: string): string {
  return humanizeIdentifier(name) || 'unnamed capability';
}
