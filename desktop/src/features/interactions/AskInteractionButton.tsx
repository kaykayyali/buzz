import { ListChecks } from "lucide-react";
import * as React from "react";

import { useFeatureEnabled } from "@/shared/features";
import { Button } from "@/shared/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/shared/ui/tooltip";
import { AskInteractionDialog } from "./AskInteractionDialog";

/**
 * Composer action that opens the prompt authoring dialog. Renders nothing
 * unless the "Interaction cards" experimental feature is enabled, so default
 * builds keep their toolbar unchanged.
 */
export const AskInteractionButton = React.memo(function AskInteractionButton({
  channelId,
  disabled = false,
}: {
  channelId: string | null;
  disabled?: boolean;
}) {
  const enabled = useFeatureEnabled("interactions");
  const [open, setOpen] = React.useState(false);
  if (!enabled) return null;
  return (
    <>
      <Tooltip disableHoverableContent>
        <TooltipTrigger asChild>
          <Button
            aria-label="Ask for a decision"
            data-testid="ask-interaction"
            disabled={disabled || !channelId}
            onClick={() => setOpen(true)}
            size="icon"
            type="button"
            variant="ghost"
          >
            <ListChecks />
          </Button>
        </TooltipTrigger>
        <TooltipContent>Ask for a decision</TooltipContent>
      </Tooltip>
      <AskInteractionDialog
        channelId={channelId}
        open={open}
        onOpenChange={setOpen}
      />
    </>
  );
});
