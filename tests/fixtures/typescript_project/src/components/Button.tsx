/**
 * Button component.
 * Demonstrates: TSX syntax, React patterns, type-only imports, props interface.
 */

import type { ReactNode, MouseEvent } from "react";

export type ButtonVariant = "primary" | "secondary" | "danger";
export type ButtonSize = "sm" | "md" | "lg";

export interface ButtonProps {
  children: ReactNode;
  variant?: ButtonVariant;
  size?: ButtonSize;
  disabled?: boolean;
  loading?: boolean;
  onClick?: (event: MouseEvent<HTMLButtonElement>) => void;
  className?: string;
  type?: "button" | "submit" | "reset";
}

const VARIANT_CLASSES: Record<ButtonVariant, string> = {
  primary: "bg-blue-500 text-white hover:bg-blue-600",
  secondary: "bg-gray-200 text-gray-800 hover:bg-gray-300",
  danger: "bg-red-500 text-white hover:bg-red-600",
};

const SIZE_CLASSES: Record<ButtonSize, string> = {
  sm: "px-2 py-1 text-sm",
  md: "px-4 py-2 text-base",
  lg: "px-6 py-3 text-lg",
};

export function Button({
  children,
  variant = "primary",
  size = "md",
  disabled = false,
  loading = false,
  onClick,
  className = "",
  type = "button",
}: ButtonProps): JSX.Element {
  const variantClass = VARIANT_CLASSES[variant];
  const sizeClass = SIZE_CLASSES[size];
  const disabledClass = disabled || loading ? "opacity-50 cursor-not-allowed" : "";

  return (
    <button
      type={type}
      className={`${variantClass} ${sizeClass} ${disabledClass} rounded font-medium ${className}`.trim()}
      disabled={disabled || loading}
      onClick={onClick}
    >
      {loading ? "Loading..." : children}
    </button>
  );
}

export default Button;
