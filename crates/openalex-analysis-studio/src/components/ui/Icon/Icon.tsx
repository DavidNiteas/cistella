import type { LucideIcon } from 'lucide-react';

export type IconSize = 12 | 14 | 16 | 18 | 20 | 24 | 32 | 40;

export interface IconProps {
  icon: LucideIcon;
  size?: IconSize;
  className?: string;
  ariaHidden?: boolean;
}

export function Icon({ icon: LucideIconComponent, size = 16, className, ariaHidden = true }: IconProps) {
  return <LucideIconComponent size={size} className={className} aria-hidden={ariaHidden} />;
}
