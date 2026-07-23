import { Settings } from "lucide-react";
import { Link } from "react-router-dom";
import { JanusLogo } from "../brand/JanusLogo";
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";
import { ButtonGroup, ButtonGroupItem } from "../ui/button-group";

export interface TopNavProps {
  readonly onSettingsClick: () => void;
}

/**
 * TopNav — quiet global header for list-style pages (home).
 * Flat and grounded (no floating card / shadow / backdrop-blur / hover-lift):
 * logo + brand + mode switcher on the left, Settings on the right, aligned to
 * the page container.
 */
export function TopNav({ onSettingsClick }: TopNavProps) {
  return (
    <nav className="container mx-auto flex h-14 items-center gap-4 px-6">
      {/* Left side: Logo + Brand + Mode switcher */}
      <div className="flex items-center gap-4">
        {/* Logo and brand */}
        <Link
          to="/"
          className="flex items-center gap-2 transition-opacity hover:opacity-80"
        >
          <JanusLogo size={28} />
          <span className="text-lg font-semibold text-foreground">Janus</span>
        </Link>

        {/* Code/MTC switcher */}
        <ButtonGroup>
          <ButtonGroupItem selected>Code</ButtonGroupItem>
          <ButtonGroupItem disabled>
            <span>MTC</span>
            <Badge tone="neutral" className="text-[10px] leading-none">
              soon
            </Badge>
          </ButtonGroupItem>
        </ButtonGroup>
      </div>

      {/* Right side: Settings button */}
      <Button
        variant="ghost"
        size="md"
        onClick={onSettingsClick}
        className="ml-auto"
      >
        <Settings className="h-4 w-4" />
        <span>Settings</span>
      </Button>
    </nav>
  );
}
