import type * as React from "react";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";
import { HoverCard, HoverCardContent, HoverCardTrigger } from "@/components/ui/hover-card";
import { cn } from "@/lib/utils";

export interface User {
  id: string;
  name: string;
  picture?: string;
  color?: string;
}

export interface PresenceBookmarksProps {
  users: User[];
  limit?: number;
  className?: string;
  renderUserContent?: (user: User) => React.ReactNode;
}

function getInitials(name: string): string {
  return name
    .split(" ")
    .map((part) => part[0])
    .join("")
    .toUpperCase()
    .slice(0, 2);
}

function DefaultUserContent({ user }: { user: User }) {
  return (
    <div className="flex items-center gap-3">
      <Avatar size="lg">
        <AvatarImage src={user.picture} alt={user.name} />
        <AvatarFallback>{getInitials(user.name)}</AvatarFallback>
      </Avatar>
      <div className="space-y-1">
        <p className="text-sm font-medium leading-none">{user.name}</p>
      </div>
    </div>
  );
}

export function PresenceBookmarks({
  users,
  limit = 5,
  className,
  renderUserContent,
}: PresenceBookmarksProps) {
  if (users.length === 0) {
    return null;
  }

  const visibleUsers = users.slice(0, limit);
  const overflowCount = users.length - limit;

  return (
    <div className={cn("flex items-center", className)} data-slot="presence-bookmarks">
      {visibleUsers.map((user, index) => (
        <HoverCard key={user.id} openDelay={200} closeDelay={100}>
          <HoverCardTrigger asChild>
            <button
              type="button"
              className={cn(
                "relative rounded-full focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-ring",
                index > 0 && "-ml-2",
              )}
              style={
                user.color
                  ? {
                      boxShadow: `0 0 0 2px ${user.color}`,
                    }
                  : undefined
              }
            >
              <Avatar size="sm" className={!user.color ? "ring-2 ring-border" : undefined}>
                <AvatarImage src={user.picture} alt={user.name} />
                <AvatarFallback>{getInitials(user.name)}</AvatarFallback>
              </Avatar>
            </button>
          </HoverCardTrigger>
          <HoverCardContent className="w-auto">
            {renderUserContent ? renderUserContent(user) : <DefaultUserContent user={user} />}
          </HoverCardContent>
        </HoverCard>
      ))}
      {overflowCount > 0 && (
        <div
          className={cn(
            "-ml-2 flex h-6 min-w-6 items-center justify-center rounded-full bg-muted px-1.5 text-xs font-medium text-muted-foreground ring-2 ring-background",
          )}
        >
          +{overflowCount}
        </div>
      )}
    </div>
  );
}
