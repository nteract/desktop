import * as React from "react";
import {
  Avatar,
  AvatarFallback,
  AvatarGroup,
  AvatarGroupCount,
  AvatarImage,
} from "@/components/ui/avatar";
import { HoverCard, HoverCardContent, HoverCardTrigger } from "@/components/ui/hover-card";

export interface CollaboratorUser {
  id: string;
  name: string;
  picture?: string;
  color?: string;
}

export interface CollaboratorAvatarsProps {
  users: CollaboratorUser[];
  currentUserId?: string;
  limit?: number;
  className?: string;
  size?: "default" | "sm" | "lg";
}

function getInitials(name: string): string {
  return name
    .split(" ")
    .map((part) => part[0])
    .join("")
    .toUpperCase()
    .slice(0, 2);
}

export function CollaboratorAvatars({
  users,
  currentUserId,
  limit = 3,
  className,
  size = "sm",
}: CollaboratorAvatarsProps) {
  // Filter out current user from display
  const displayUsers = React.useMemo(() => {
    return currentUserId ? users.filter((user) => user.id !== currentUserId) : users;
  }, [users, currentUserId]);

  if (displayUsers.length === 0) {
    return null;
  }

  const visibleUsers = displayUsers.slice(0, limit);
  const overflowCount = displayUsers.length - limit;
  const overflowUsers = displayUsers.slice(limit);

  return (
    <AvatarGroup data-slot="collaborator-avatars" className={className}>
      {visibleUsers.map((user) => (
        <HoverCard key={user.id} openDelay={200} closeDelay={100}>
          <HoverCardTrigger asChild>
            <Avatar
              size={size}
              className="cursor-pointer transition-transform hover:scale-110 hover:z-10"
              style={user.color ? { boxShadow: `0 0 0 2px ${user.color}` } : undefined}
            >
              {user.picture ? <AvatarImage src={user.picture} alt={user.name} /> : null}
              <AvatarFallback
                style={user.color ? { backgroundColor: user.color, color: "white" } : undefined}
              >
                {getInitials(user.name)}
              </AvatarFallback>
            </Avatar>
          </HoverCardTrigger>
          <HoverCardContent className="w-auto min-w-[120px] p-3">
            <div className="flex items-center gap-2">
              <Avatar size="default">
                {user.picture ? <AvatarImage src={user.picture} alt={user.name} /> : null}
                <AvatarFallback
                  style={user.color ? { backgroundColor: user.color, color: "white" } : undefined}
                >
                  {getInitials(user.name)}
                </AvatarFallback>
              </Avatar>
              <span className="text-sm font-medium">{user.name}</span>
            </div>
          </HoverCardContent>
        </HoverCard>
      ))}
      {overflowCount > 0 && (
        <HoverCard openDelay={200} closeDelay={100}>
          <HoverCardTrigger asChild>
            <AvatarGroupCount className="cursor-pointer transition-transform hover:scale-110 hover:z-10">
              +{overflowCount}
            </AvatarGroupCount>
          </HoverCardTrigger>
          <HoverCardContent className="w-auto min-w-[150px] p-3">
            <div className="space-y-2">
              {overflowUsers.map((user) => (
                <div key={user.id} className="flex items-center gap-2">
                  <Avatar size="sm">
                    {user.picture ? <AvatarImage src={user.picture} alt={user.name} /> : null}
                    <AvatarFallback
                      style={
                        user.color ? { backgroundColor: user.color, color: "white" } : undefined
                      }
                    >
                      {getInitials(user.name)}
                    </AvatarFallback>
                  </Avatar>
                  <span className="text-sm">{user.name}</span>
                </div>
              ))}
            </div>
          </HoverCardContent>
        </HoverCard>
      )}
    </AvatarGroup>
  );
}
