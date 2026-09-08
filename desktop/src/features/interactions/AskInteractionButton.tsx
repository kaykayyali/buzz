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
 *
 * The dialog is pinned to the channel it was opened for: it publishes only
 * to that channel, closes if the active channel changes underneath it, and
 * its draft is keyed per channel so text written for one channel is never
 * carried into another.
 */
export const AskInteractionButton = React.memo(function AskInteractionButton({
  channelId,
  disabled = false,
}: {
  channelId: string | null;
  disabled?: boolean;
}) {
  const enabled = useFeatureEnabled("interactions");
  const [openFor, setOpenFor] = React.useState<string | null>(null);
  if (!enabled) return null;
  const open = openFor !== null && openFor === channelId;
  return (
    <>
      <Tooltip disableHoverableContent>
        <TooltipTrigger asChild>
          <Button
            aria-label="Ask for a decision"
            data-testid="ask-interaction"
            disabled={disabled || !channelId}
            onClick={() => setOpenFor(channelId)}
            size="icon"
            type="button"
            variant="ghost"
          >
            <ListChecks />
          </Button>
        </TooltipTrigger>
        <TooltipContent>Ask for a decision</TooltipContent>
      </Tooltip>
      {channelId ? (
        <AskInteractionDialog
          key={channelId}
          channelId={channelId}
          open={open}
          onOpenChange={(next) => setOpenFor(next ? channelId : null)}
        />
      ) : null}
    </>
  );
});
