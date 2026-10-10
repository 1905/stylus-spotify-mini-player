// Library find: the list filter and the Library search, over lists already on the screen or in the disk cache.

/** The row of uri is the song that plays now. */
export const isNowRow = (uri, now) => Boolean(uri && now && now.uri === uri);
