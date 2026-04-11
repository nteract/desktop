import { ShieldAlert } from "lucide-react";
import { Button } from "@/components/ui/button";

interface UntrustedBannerProps {
  onReviewClick: () => void;
}

export function UntrustedBanner({ onReviewClick }: UntrustedBannerProps) {
  return (
    <div className="flex items-center justify-center gap-3 bg-amber-500/90 px-3 py-1.5 text-xs text-amber-950">
      <ShieldAlert className="h-4 w-4" />
      <span>This notebook has dependencies that need approval before the kernel can start.</span>
      <Button
        size="sm"
        variant="secondary"
        className="h-6 px-2 text-xs bg-amber-100 hover:bg-amber-200 text-amber-900"
        data-testid="review-dependencies-button"
        onClick={onReviewClick}
      >
        Review Dependencies
      </Button>
    </div>
  );
}
