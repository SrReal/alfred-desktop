import { AssetRegistry } from "../assets";

// Decorative images retain the surrounding control's accessible text.
export function PixelIcon({ name }: { name: string }) {
  const source = AssetRegistry.icons[name];
  return source ? (
    <img className="pixel-icon" src={source} alt="" aria-hidden="true" />
  ) : null;
}
