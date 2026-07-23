import { z } from "zod";

export const isoDateTimeSchema = z.string().datetime();
