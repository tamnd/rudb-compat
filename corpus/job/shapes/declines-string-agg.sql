-- declines: not a MIN or a MAX
SELECT length(string_agg(k.keyword, ',')) AS width FROM keyword AS k, movie_keyword AS mk WHERE k.id = mk.keyword_id;
